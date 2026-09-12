const fs = require('node:fs');

/** Reserve fresh evidence before launching; never replace another run's report. */
function initializePerformanceReport(filename, report) {
  try {
    fs.lstatSync(`${filename}.pending`);
    throw new Error('A pending browser performance report already exists');
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
  }
  fs.writeFileSync(filename, JSON.stringify(report, null, 2), { flag: 'wx', mode: 0o600 });
}

/** A failed rewrite must not truncate the earlier, explicitly pending report. */
function persistPerformanceReport(filename, report) {
  const pending = `${filename}.pending`;
  fs.writeFileSync(pending, JSON.stringify(report, null, 2), { flag: 'wx', mode: 0o600 });
  fs.renameSync(pending, filename);
}

/** Retain a nonpassing snapshot before cleanup; preserve the workload failure. */
async function finalizePerformanceReport(
  report,
  closeBrowser,
  persist,
  reportError = console.error,
) {
  const workloadPassed = report.completed === true && report.passed === true;
  report.passed = false;
  report.finalization = { completed: false, browserCleanupCompleted: false, errors: [] };
  const recordError = (step, error) => {
    report.finalization.errors.push(`${step}: ${error?.message || String(error)}`);
    report.passed = false;
  };
  try {
    await persist(report);
  } catch (error) {
    recordError('Initial report write', error);
  }
  try {
    await closeBrowser();
    report.finalization.browserCleanupCompleted = true;
  } catch (error) {
    recordError('Browser cleanup', error);
  }
  report.finalization.completed = true;
  report.finishedAt = new Date().toISOString();
  report.passed = workloadPassed && report.finalization.errors.length === 0;
  try {
    await persist(report);
  } catch (error) {
    recordError('Final report write', error);
  }
  if (report.finalization.errors.length && !Object.hasOwn(report, 'failure')) {
    throw new Error(report.finalization.errors.join('; '));
  }
  for (const error of report.finalization.errors) {
    // The original workload exception stays primary even if stderr also fails.
    try {
      reportError(error);
    } catch {}
  }
}

module.exports = {
  initializePerformanceReport,
  finalizePerformanceReport,
  persistPerformanceReport,
};
