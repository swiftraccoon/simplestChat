const fs = require('node:fs');
const path = require('node:path');

/** A bounded runner trace: explicit scenario labels, status and timing only.
 * Never serialize API parameters, error call logs, page content or network data. */
class SafeReporter {
  constructor({ artifacts }) {
    this.artifacts = artifacts;
    this.results = [];
    this.steps = [];
  }
  onBegin(_config, suite) {
    console.log(`Browser runner: ${suite.allTests().length} selected tests.`);
  }
  onStepEnd(test, result, step) {
    if (step.category !== 'test.step' || this.steps.length >= 10000) return;
    console.log(`${step.error ? 'FAIL' : 'PASS'} ${step.title}`);
    this.steps.push({
      project: test.parent.project().name,
      test: test.title,
      retry: result.retry,
      step: step.title,
      durationMs: step.duration,
      passed: !step.error,
    });
  }
  onTestEnd(test, result) {
    console.log(`${result.status.toUpperCase()} ${test.parent.project().name}: ${test.title}`);
    this.results.push({
      project: test.parent.project().name,
      test: test.title,
      retry: result.retry,
      status: result.status,
      durationMs: result.duration,
      errors: result.errors.length,
    });
  }
  onEnd(result) {
    const report = { status: result.status, tests: this.results, steps: this.steps };
    fs.writeFileSync(
      path.join(this.artifacts, 'runner-results.json'),
      JSON.stringify(report, null, 2) + '\n',
      { mode: 0o600 },
    );
    fs.writeFileSync(
      path.join(this.artifacts, 'step-trace.json'),
      JSON.stringify(this.steps, null, 2) + '\n',
      { mode: 0o600 },
    );
    const escape = (text) =>
      String(text).replace(
        /[&<>"']/g,
        (char) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[char],
      );
    const rows = this.results
      .map(
        (item) =>
          `<tr><td>${escape(item.project)}</td><td>${escape(item.test)}</td><td>${escape(item.status)}</td><td>${item.durationMs}</td><td>${item.retry}</td></tr>`,
      )
      .join('');
    fs.writeFileSync(
      path.join(this.artifacts, 'runner-results.html'),
      `<!doctype html><meta charset="utf-8"><title>Browser test results</title><h1>${escape(result.status)}</h1><table><thead><tr><th>Project</th><th>Test</th><th>Status</th><th>Milliseconds</th><th>Retry</th></tr></thead><tbody>${rows}</tbody></table>`,
      { mode: 0o600 },
    );
    console.log(`Private runner reports: ${this.artifacts}`);
  }
}
module.exports = SafeReporter;
