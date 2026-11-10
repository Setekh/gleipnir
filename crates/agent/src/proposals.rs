//! Edits an agent offers for review. Each is shown as a diff of the open file against what's
//! proposed, where the person accepts or rejects each change, once nothing else is popped up over
//! the window, one at a time and in the order they came.
//!
//! The agent's call waits for the decision, but only so long: agents give up on tool calls after a
//! while (Codex after a minute, unless configured otherwise), and a person may take longer. So a
//! call that runs out of time answers "pending" with the proposal's id, and `get_proposal` waits
//! again. The review stays open meanwhile, and what's decided is kept until asked for.

use crate::Agent;
use crate::ide::{self, CLOSED};
use crate::rpc::ToolOutput;
use crate::tools::Edit;
use gleipnir_core::BufferModel;
use gleipnir_git_ui::{DiffView, ReviewDecision, ReviewEvent};
use gleipnir_workspace::Workspace;
use gpui::{AsyncWindowContext, Context, Entity, Subscription, WeakEntity, Window};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::time::Duration;

/// How often the queue is looked at, to show the next proposal once the window is free.
const QUEUE_POLL: Duration = Duration::from_millis(150);

#[derive(Default)]
pub struct Proposals {
    next_id: u64,
    items: HashMap<u64, Proposal>,
    /// Waiting to be shown, oldest first.
    queue: VecDeque<u64>,
    showing: Option<u64>,
    /// Whether something is watching for the window to be free to show the next.
    watching: bool,
}

struct Proposal {
    edit: Edit,
    buffer: Entity<BufferModel>,
    state: State,
    _subscriptions: Vec<Subscription>,
}

enum State {
    Waiting,
    Reviewing,
    Finished {
        decisions: Vec<ReviewDecision>,
        undecided: usize,
        note: Option<String>,
    },
}

impl Proposals {
    fn finish(
        &mut self,
        id: u64,
        decisions: Vec<ReviewDecision>,
        undecided: usize,
        note: Option<String>,
    ) {
        if self.showing == Some(id) {
            self.showing = None;
        }
        if let Some(proposal) = self.items.get_mut(&id)
            && !matches!(proposal.state, State::Finished { .. })
        {
            proposal.state = State::Finished {
                decisions,
                undecided,
                note,
            };
        }
    }

    fn is_finished(&self, id: u64) -> Option<bool> {
        Some(matches!(self.items.get(&id)?.state, State::Finished { .. }))
    }

    /// What's known of a proposal, as the agent is told it.
    fn describe(&self, id: u64) -> Option<Value> {
        let proposal = self.items.get(&id)?;
        let path = &proposal.edit.path.relative;
        Some(match &proposal.state {
            State::Waiting | State::Reviewing => {
                let shown = matches!(proposal.state, State::Reviewing);
                json!({
                    "id": id,
                    "path": path,
                    "status": "pending",
                    "message": if shown {
                        "The person is reviewing it. Call get_proposal with this id to wait for their decision."
                    } else {
                        "It's waiting to be shown, after other popups or proposals. Call get_proposal with this id to wait for the person's decision."
                    },
                })
            }
            State::Finished {
                decisions,
                undecided,
                note,
            } => {
                let accepted = decisions.iter().filter(|d| d.accepted).count();
                let rejected = decisions.len() - accepted + undecided;
                let status = match (accepted, rejected) {
                    (0, 0) => "unchanged",
                    (_, 0) => "accepted",
                    (0, _) => "rejected",
                    _ => "partly accepted",
                };
                let changes: Vec<Value> = decisions
                    .iter()
                    .map(|d| {
                        json!({
                            "decision": if d.accepted { "accepted" } else { "rejected" },
                            "line": d.line,
                            "removed": d.removed,
                            "added": d.added,
                        })
                    })
                    .collect();
                let mut answer = json!({
                    "id": id,
                    "path": path,
                    "status": status,
                    "accepted": accepted,
                    "rejected": rejected,
                    "changes": changes,
                });
                if *undecided > 0 {
                    answer["undecided"] = json!(undecided);
                    answer["message"] = json!(format!(
                        "{undecided} change(s) were left out undecided: the person closed the \
                         review first, or the file changed so they no longer fit it."
                    ));
                }
                if accepted > 0 {
                    answer["saved"] = json!(false);
                }
                if let Some(note) = note {
                    answer["message"] = json!(note);
                }
                answer
            }
        })
    }
}

/// Offers an edit for review, and waits up to `wait` for the person's decision.
pub(crate) async fn propose(
    workspace: &WeakEntity<Workspace>,
    agent: &WeakEntity<Agent>,
    edit: Edit,
    wait: Duration,
    cx: &mut AsyncWindowContext,
) -> Result<ToolOutput, String> {
    let buffer = ide::open_in_background(workspace, &edit.path, cx).await?;
    // Said now if it can't apply, rather than after a wait for review.
    let text = buffer.read_with(cx, |b, _| b.buffer().text());
    ide::find_edits(&text, &edit)?;
    let id = agent
        .update(cx, |agent, _| {
            let proposals = &mut agent.proposals;
            proposals.next_id += 1;
            let id = proposals.next_id;
            proposals.items.insert(
                id,
                Proposal {
                    edit,
                    buffer,
                    state: State::Waiting,
                    _subscriptions: Vec::new(),
                },
            );
            proposals.queue.push_back(id);
            id
        })
        .map_err(|_| CLOSED.to_string())?;
    watch_queue(workspace, agent, cx);
    answer(agent, id, wait, cx).await
}

/// What the person decided, waiting up to `wait` if they haven't yet.
pub(crate) async fn answer(
    agent: &WeakEntity<Agent>,
    id: u64,
    wait: Duration,
    cx: &mut AsyncWindowContext,
) -> Result<ToolOutput, String> {
    let finished = |cx: &mut AsyncWindowContext| {
        agent
            .read_with(cx, |agent, _| agent.proposals.is_finished(id))
            .map_err(|_| CLOSED.to_string())?
            .ok_or_else(|| format!("There's no proposal with id {id} in this window"))
    };
    ide::wait_until(cx, wait, finished).await?;
    agent
        .read_with(cx, |agent, _| agent.proposals.describe(id))
        .map_err(|_| CLOSED.to_string())?
        .map(ToolOutput::Json)
        .ok_or_else(|| format!("There's no proposal with id {id} in this window"))
}

/// Shows queued proposals one after another as the window is free, until there are none.
fn watch_queue(
    workspace: &WeakEntity<Workspace>,
    agent: &WeakEntity<Agent>,
    cx: &mut AsyncWindowContext,
) {
    let already = agent
        .update(cx, |agent, _| {
            std::mem::replace(&mut agent.proposals.watching, true)
        })
        .unwrap_or(true);
    if already {
        return;
    }
    let (workspace, agent) = (workspace.clone(), agent.clone());
    cx.spawn(async move |cx| {
        loop {
            let more = workspace
                .update_in(cx, |workspace, window, cx| {
                    show_next(workspace, &agent, window, cx)
                })
                .unwrap_or(false);
            if !more {
                agent
                    .update(cx, |agent, _| agent.proposals.watching = false)
                    .ok();
                return;
            }
            cx.background_executor().timer(QUEUE_POLL).await;
        }
    })
    .detach();
}

/// Shows the next proposal if nothing else is up. Returns whether any are still waiting or shown.
fn show_next(
    workspace: &mut Workspace,
    agent: &WeakEntity<Agent>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> bool {
    let Some(agent) = agent.upgrade() else {
        return false;
    };
    let (showing, next) = {
        let proposals = &agent.read(cx).proposals;
        (proposals.showing, proposals.queue.front().copied())
    };
    if showing.is_some() {
        return true;
    }
    let Some(id) = next else {
        return false;
    };
    // The person's own popup, like Search Everywhere, isn't taken away from them.
    if workspace.has_modal() {
        return true;
    }
    agent.update(cx, |agent, _| agent.proposals.queue.pop_front());
    let Some((edit, buffer)) = agent
        .read(cx)
        .proposals
        .items
        .get(&id)
        .map(|p| (p.edit.clone(), p.buffer.clone()))
    else {
        return true;
    };
    // The file may have changed while the proposal waited.
    let now = buffer.read(cx).buffer().text();
    let proposed = match ide::find_edits(&now, &edit) {
        Ok(edits) => ide::apply_edits(&now, &edits),
        Err(why) => {
            let note = format!(
                "The file changed before the proposal could be shown, and it no longer applies: {why}"
            );
            agent.update(cx, |agent, _| {
                agent.proposals.finish(id, Vec::new(), 0, Some(note))
            });
            return true;
        }
    };
    let title = format!("{} (proposed by an agent)", edit.path.relative);
    let review_buffer = buffer.clone();
    let view = workspace.toggle_modal(window, cx, move |_, cx| {
        DiffView::new(title, ("Now", &now), ("Proposed", &proposed), cx).reviewing(review_buffer)
    });
    let Some(view) = view else {
        // Can't happen with no popup open; tried again next time if it does.
        agent.update(cx, |agent, _| agent.proposals.queue.push_front(id));
        return true;
    };
    if view.read(cx).change_rows().is_empty() {
        workspace.dismiss_modal(window, cx);
        let note = "Nothing to review: the proposal only differs from the file in line endings or \
                    the final line break."
            .to_string();
        agent.update(cx, |agent, _| {
            agent.proposals.finish(id, Vec::new(), 0, Some(note))
        });
        return true;
    }
    agent.update(cx, |agent, cx| {
        let subscriptions = vec![
            cx.subscribe(
                &view,
                move |agent: &mut Agent, _, event: &ReviewEvent, _| {
                    let ReviewEvent::Finished {
                        decisions,
                        undecided,
                    } = event;
                    agent
                        .proposals
                        .finish(id, decisions.clone(), *undecided, None);
                },
            ),
            // Closed some other way, as by the window closing.
            cx.observe_release(&view, move |agent: &mut Agent, view: &mut DiffView, _| {
                let undecided = view.change_rows().len();
                let decisions = view.decisions().to_vec();
                agent.proposals.finish(id, decisions, undecided, None);
            }),
        ];
        agent.proposals.showing = Some(id);
        if let Some(proposal) = agent.proposals.items.get_mut(&id) {
            proposal.state = State::Reviewing;
            proposal._subscriptions = subscriptions;
        }
    });
    true
}
