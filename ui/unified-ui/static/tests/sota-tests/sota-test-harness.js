(function () {
  function normalizeStatus(status) {
    if (status === "pass" || status === "fail" || status === "running") {
      return status;
    }
    return "pending";
  }

  function counts() {
    return Array.from(document.querySelectorAll(".test-case")).reduce(
      (acc, testCase) => {
        acc.total += 1;
        acc[normalizeStatus(testCase.dataset.status)] += 1;
        return acc;
      },
      { total: 0, pass: 0, fail: 0, pending: 0, running: 0 }
    );
  }

  function updateSummary() {
    const summary = counts();
    const passed = document.getElementById("passed-count");
    const failed = document.getElementById("failed-count");
    const pending = document.getElementById("pending-count");
    const running = document.getElementById("running-count");
    if (passed) passed.textContent = String(summary.pass);
    if (failed) failed.textContent = String(summary.fail);
    if (pending) pending.textContent = String(summary.pending + summary.running);
    if (running) running.textContent = String(summary.running);
    return summary;
  }

  function findTestCase(testId) {
    return (
      document.getElementById(testId) ||
      Array.from(document.querySelectorAll(".test-case")).find(
        (testCase) => testCase.dataset.testId === testId
      ) ||
      null
    );
  }

  function setCheckState(testId, checkName, state) {
    const testCase = findTestCase(testId);
    if (!testCase) return;
    const check = testCase.querySelector(`[data-check="${checkName}"]`);
    if (!check) return;
    const normalized = normalizeStatus(state);
    check.setAttribute("data-check-state", normalized);
    // Evaluate auto-pass SYNCHRONOUSLY here (in addition to the MutationObserver
    // backstop in installAutoPass) so a caller that completes a test and reads
    // #passed-count in the SAME tick sees the up-to-date status — otherwise the
    // count only flips on the observer's later tick. maybeAutoPass is idempotent
    // and flips only when ALL [data-check] are satisfied, so this never over-fires.
    maybeAutoPass(testCase);
  }

  function setResult(testId, text, tone) {
    const testCase = findTestCase(testId);
    if (!testCase) return;
    const result = testCase.querySelector(".fixture-result");
    if (!result) return;
    result.textContent = text;
    result.className = `fixture-result ${tone || "info"}`;
  }

  function setTestStatus(testId, status) {
    const testCase = findTestCase(testId);
    if (!testCase) return;
    testCase.dataset.status = normalizeStatus(status);
    updateSummary();
  }

  // ---- Auto-pass: a test passes when ALL its [data-check] items are satisfied
  // (the page's own genuine-interaction detection), so there is no manual
  // Pass/Fail control to game. Supports the three completion conventions used
  // across the suite: data-check-state attribute, inline strike-through styling,
  // and a done/checked/complete class on the check element.
  var DONE_RE = /\b(done|checked|complete|completed|passed|success|ok)\b/;

  function checkSatisfied(check) {
    var state = (check.getAttribute("data-check-state") || "").toLowerCase();
    if (state === "pass" || state === "done" || state === "complete" ||
        state === "completed" || state === "success" || state === "checked") {
      return true;
    }
    var inline = (check.style && check.style.textDecoration) || "";
    if (inline.indexOf("line-through") !== -1) return true;
    try {
      var computed = window.getComputedStyle(check);
      var line = (computed.textDecorationLine || computed.textDecoration || "");
      if (line.indexOf("line-through") !== -1) return true;
    } catch (_) { /* getComputedStyle can throw on detached nodes */ }
    if (DONE_RE.test(check.className || "")) return true;
    return false;
  }

  function maybeAutoPass(testCase) {
    if (!testCase || testCase.dataset.status === "pass") return;
    var checks = Array.from(testCase.querySelectorAll("[data-check]"));
    if (checks.length === 0) return; // no completion signal — not an auto-pass test
    if (checks.every(checkSatisfied)) {
      testCase.dataset.status = "pass";
      updateSummary();
    }
  }

  function installAutoPass() {
    var testCases = Array.from(document.querySelectorAll(".test-case"));
    testCases.forEach(maybeAutoPass); // catch already-satisfied state on load
    if (typeof MutationObserver === "undefined") return;
    var observer = new MutationObserver(function (mutations) {
      var seen = new Set();
      for (var i = 0; i < mutations.length; i++) {
        var node = mutations[i].target;
        var el = node && node.nodeType === 1 ? node : node && node.parentElement;
        var tc = el && el.closest ? el.closest(".test-case") : null;
        if (tc && !seen.has(tc)) {
          seen.add(tc);
          maybeAutoPass(tc);
        }
      }
    });
    testCases.forEach(function (tc) {
      observer.observe(tc, {
        attributes: true,
        attributeFilter: ["data-check-state", "style", "class"],
        subtree: true,
        childList: true,
        characterData: true,
      });
    });
  }

  // ---- Manual Pass/Fail radios are removed from the HTML of every test whose
  // completion is auto-detected (auto-pass is the source of truth there). A few
  // tests have sub-cases that genuinely cannot be auto-detected — cross-origin
  // iframe content, external sandboxes, pure detect-and-report — and keep their
  // manual radios; wire those so they still function. Fully-converted tests have
  // no [data-status-radio] markup left, so this is a harmless no-op for them.
  function wireManualStatusRadios() {
    document.querySelectorAll("[data-status-radio]").forEach(function (input) {
      input.addEventListener("change", function () {
        if (!input.checked) return;
        var tc = input.closest ? input.closest(".test-case") : null;
        if (!tc) return;
        tc.dataset.status = normalizeStatus(input.value);
        updateSummary();
      });
    });
  }

  function resetTestCase(testCase) {
    if (!testCase) return;
    testCase.dataset.status = "pending";
    Array.from(testCase.querySelectorAll("[data-check]")).forEach(function (check) {
      check.setAttribute("data-check-state", "pending");
      if (check.style) {
        check.style.textDecoration = "";
        check.style.color = "";
      }
      if (check.className) {
        check.className = check.className.replace(DONE_RE, "").replace(/\s+/g, " ").trim();
      }
    });
    var result = testCase.querySelector(".fixture-result");
    if (result) {
      result.textContent = "";
      result.className = "fixture-result";
    }
    updateSummary();
  }

  function resetAll() {
    Array.from(document.querySelectorAll(".test-case")).forEach(resetTestCase);
    updateSummary();
  }

  function wireResetControls() {
    document.querySelectorAll("[data-status-reset]").forEach(function (btn) {
      btn.addEventListener("click", function () {
        var id = btn.getAttribute("data-status-reset");
        var tc = (id && findTestCase(id)) || (btn.closest && btn.closest(".test-case"));
        resetTestCase(tc);
      });
    });
  }

  function init() {
    wireManualStatusRadios();
    wireResetControls();
    installAutoPass();
    updateSummary();
  }

  window.setCheckState = setCheckState;
  window.setResult = setResult;
  window.setTestStatus = setTestStatus;
  window.resetAllTests = resetAll;

  window.addEventListener("DOMContentLoaded", init);
  window.addEventListener("load", updateSummary);
})();
