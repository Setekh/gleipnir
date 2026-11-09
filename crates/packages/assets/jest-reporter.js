// Reports each test to Gleipnir as it finishes, alongside jest's own reporter, so the results
// tree fills in as the run goes. One line per event on stdout, after a mark Gleipnir looks for.
const MARK = '\u001fgleipnir ';

class GleipnirReporter {
  emit(event) {
    process.stdout.write(MARK + JSON.stringify(event) + '\n');
  }

  onTestFileStart(test) {
    this.emit({ event: 'file', path: test.path });
  }

  onTestCaseResult(test, result) {
    this.emit({
      event: 'test',
      path: test.path,
      ancestorTitles: result.ancestorTitles,
      title: result.title,
      status: result.status,
      duration: result.duration,
      failureMessages: result.failureMessages,
      location: result.location,
    });
  }

  onTestFileResult(test, result) {
    const error = result.testExecError;
    this.emit({
      event: 'fileDone',
      path: test.path,
      duration: result.perfStats ? result.perfStats.end - result.perfStats.start : undefined,
      // A file that couldn't run at all, like one that doesn't compile.
      error: error ? result.failureMessage || error.stack || error.message || String(error) : undefined,
    });
  }
}

module.exports = GleipnirReporter;
