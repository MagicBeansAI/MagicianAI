#!/usr/bin/env python3
"""The live Task Recipes cases (p1..p6) and their oracles.

A case is now data + a small function, edited and run without a rebuild. Each
case drives the live agent once to learn a task by browser, then proves the
same task runs again over the site's API with no browser. The oracle for the
public read cases is an independent HTTP fetch this file makes itself; the
signed-in cases (p5 gmail, p6 keka) compare the browserless replay against what
the browser itself reported, since their data cannot be fetched anonymously.
"""

from __future__ import annotations

import json
import os
import ssl
import time
import urllib.request
from pathlib import Path

# macOS framework Python cannot find the system CA store, so HTTPS to public
# sites fails cert verification. certifi carries a bundle; fall back to the
# default context if it is somehow absent.
try:
    import certifi
    _SSL_CTX = ssl.create_default_context(cafile=certifi.where())
except Exception:  # noqa: BLE001
    _SSL_CTX = ssl.create_default_context()

import harness as H
from harness import Case, Expect, Gate, Observed, Phase

UA = "magician-task-recipes-eval/1 (+https://github.com/MagicBeansAI/MagicianAI; read-only eval)"
RECIPE_REPLAY = "recipe_replay"
RECIPE_WAIT = 120
UI_THREAD = "task-recipes-public-eval"

ALL_CASES = ["p1", "p2", "p3", "p4"]
OPT_IN_CASES = ["p5", "p6"]  # signed-in accounts; run only when named

LIVE_PREAMBLE = (
    "API-mining evaluation against a public site. Read the page with the browser TOOL "
    "(browser__open and friends) — not web_search, not web_fetch, and not the shell: do not "
    "run curl, wget, or a browser binary yourself, because a page fetched outside the browser "
    "tool is invisible to this evaluation. These pages change between runs, so read the page "
    "now; never answer from memory of an earlier run. Never log in, never submit a form, and "
    "never modify anything on the site — every task here is read-only. This runs unattended "
    "with no human to answer questions, so never pause for user input. Report the answer as a "
    "plain value in your final summary. "
)


def describe(body: str) -> str:
    return LIVE_PREAMBLE + body


def describe_write(body: str) -> str:
    return (
        "API-mining evaluation against a signed-in site, using an account the operator has "
        "authorised. Read the page with the browser TOOL (browser__open and friends) with "
        'connection_mode "cdp" so it attaches to the already signed-in Chrome — not web_search, '
        "not web_fetch, and not the shell. Never sign in; the session is already open. Perform "
        "ONLY the single state change named below and nothing else: do not repeat it, do not "
        "touch any other control, do not navigate elsewhere, and do not read, open, reply to, or "
        "change anything else on the site. This runs unattended with no human to answer "
        f"questions, so never pause for user input. {body}"
    )


# --------------------------------------------------------------------------- #
# phase runner                                                                #
# --------------------------------------------------------------------------- #
def _approve_replay(pending):
    if not pending:
        return None
    req = pending[0]
    rid = req.get("id")
    if not rid:
        return None
    option = "approve_once"
    for opt in (req.get("options") or []):
        if opt.get("id"):
            option = opt["id"]
            break
    return rid, option


def observe(m: H.Magician, title: str, description: str, timeout: int) -> Observed:
    tabs_before, seq_before = m.browser_signals()
    task_id, execution_id = m.create_and_execute(title, description, UI_THREAD)
    status, otype, summary = m.wait_terminal(task_id, execution_id, timeout, on_pending=_approve_replay)
    tabs_after, seq_after = m.browser_signals()
    time.sleep(3)  # compile + ledger land just after terminal
    events = m.execution_events(task_id, execution_id)
    term = H.terminal_outcome(events)
    if term:
        status, otype, summary = term
    return Observed(task_id, execution_id, status, otype, summary, events,
                    tabs_before, tabs_after, seq_before, seq_after,
                    m.capture_trace_count(execution_id))


def run_phase(m, phase_id, title, description, expect, timeout, extra=None) -> Phase:
    try:
        obs = observe(m, title, description, timeout)
    except Exception as exc:  # noqa: BLE001 — a phase error is a result, not a crash
        return Phase(phase_id, error=f"{type(exc).__name__}: {exc}")
    gates = H.gates_for(obs, expect)
    if extra:
        gates.extend(extra(obs))
    p = Phase(phase_id, obs.task_id, obs.execution_id, obs.status, obs.outcome_type,
              obs.summary[:400], gates)
    return p


def bind_recipe(m: H.Magician, case: Case, task_id: str) -> dict | None:
    bound = m.wait_for_recipe_bound(task_id, RECIPE_WAIT)
    cold = case.phases[-1]
    if bound is None:
        cold.gates.append(Gate("recipe_compiled", False, "no recipe bound within 120s"))
        return None
    recipe_id, detail = bound
    case.recipe_id = recipe_id
    versions = len(detail.get("versions", [])) if isinstance(detail, dict) else 0
    cold.gates.append(Gate("recipe_compiled", True, f"versions={versions}"))
    return detail


# --------------------------------------------------------------------------- #
# oracles — independent HTTP, no browser                                      #
# --------------------------------------------------------------------------- #
def _get_json(url: str):
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    with urllib.request.urlopen(req, timeout=20, context=_SSL_CTX) as r:
        return json.loads(r.read().decode())


def _get_text(url: str) -> str:
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    with urllib.request.urlopen(req, timeout=30, context=_SSL_CTX) as r:
        return r.read().decode(errors="replace")


def hn_top_story_points(query: str) -> str:
    d = _get_json(f"https://hn.algolia.com/api/v1/search?query={query}&tags=story&hitsPerPage=1")
    return str(d["hits"][0]["points"])


def hn_top_story_title(query: str) -> str:
    d = _get_json(f"https://hn.algolia.com/api/v1/search?query={query}&tags=story&hitsPerPage=1")
    return d["hits"][0]["title"]


def open_meteo_elevation(lat: float, lon: float) -> str:
    d = _get_json(f"https://api.open-meteo.com/v1/forecast?latitude={lat}&longitude={lon}&current=temperature_2m")
    # str() of the parsed float keeps the JSON spelling (38.0), and answer_matches
    # has a numeric fallback so 38.0 still matches 38 either way.
    return str(d["elevation"])


def wikipedia_first_appeared_year(page: str) -> str:
    html = _get_text(f"https://en.wikipedia.org/api/rest_v1/page/html/{page}")
    text = _flatten_markup(html)
    idx = text.find("First appeared")
    if idx < 0:
        raise RuntimeError(f"no first-appeared row in the {page} infobox")
    tail = text[idx:idx + 120]
    year = _year_in(tail)
    if not year:
        raise RuntimeError("no year on the first-appeared row")
    return year


def _flatten_markup(html: str) -> str:
    out, in_tag, last_space = [], False, False
    for ch in html:
        if ch == "<":
            in_tag = True
            continue
        if ch == ">" and in_tag:
            in_tag = False
            c = " "
        elif in_tag:
            continue
        elif ch.isspace() or ch == "\xa0":
            c = " "
        else:
            c = ch
        if c == " ":
            if not last_space:
                out.append(" ")
            last_space = True
        else:
            out.append(c)
            last_space = False
    return "".join(out)


def _year_in(text: str) -> str | None:
    i = 0
    while i < len(text):
        if not text[i].isdigit():
            i += 1
            continue
        start = i
        while i < len(text) and text[i].isdigit():
            i += 1
        digits = text[start:i]
        if len(digits) == 4 and 1800 <= int(digits) <= 2200:
            return digits
    return None


# --------------------------------------------------------------------------- #
# drift                                                                       #
# --------------------------------------------------------------------------- #
def force_drift(m: H.Magician, recipe_id: str) -> None:
    """Point the recipe's answer extractor at a path no response carries, the
    way a site's schema change would. Edits the recipe JSON on disk."""
    path = m.scope_dir() / "api_mining" / "recipes" / f"{recipe_id}.json"
    recipe = json.loads(path.read_text())
    current = recipe["current_version"]
    for version in recipe["versions"]:
        if version.get("version") == current:
            for spec in version.get("answer_spec", []):
                ex = spec.get("extractor", {})
                if ex.get("kind") == "json_path":
                    ex["path"] = "$.__forced_task_recipe_drift__"
    path.write_text(json.dumps(recipe))


# --------------------------------------------------------------------------- #
# cases                                                                       #
# --------------------------------------------------------------------------- #
HN_TITLE = "Points of the top Hacker News story about {q}"
HN_BODY = "Open https://hn.algolia.com/?q={q} and report the points of the first story result."


def hn_case(m: H.Magician, timeout: int) -> Case:
    case = Case("p1", "hn.algolia", "https://hn.algolia.com")
    try:
        cold_points = hn_top_story_points("rust")
    except Exception as e:
        case.error = f"oracle unreachable: {e}"
        return case
    title = HN_TITLE.format(q="rust")
    desc = describe(HN_BODY.format(q="rust"))
    case.phases.append(run_phase(m, "cold", title, desc, Expect(answer=[cold_points]), timeout))
    if not case.phases[0].task_id:
        return case
    recipe = bind_recipe(m, case, case.phases[0].task_id)
    if recipe is None:
        return case
    try:
        warm_points = hn_top_story_points("rust")
    except Exception as e:
        case.error = f"oracle unreachable: {e}"
        return case
    case.phases.append(run_phase(m, "warm", title, desc,
                                 Expect(answer=[warm_points], outcome_type=RECIPE_REPLAY, browserless=True), timeout))
    try:
        variant_points = hn_top_story_points("golang")
    except Exception as e:
        case.error = f"oracle unreachable: {e}"
        return case
    vtitle = HN_TITLE.format(q="golang")
    vdesc = describe(HN_BODY.format(q="golang"))
    case.phases.append(run_phase(m, "variant", vtitle, vdesc,
                                 Expect(answer=[variant_points], outcome_type=RECIPE_REPLAY, browserless=True), timeout))
    return case


BERLIN = (52.52, 13.41)


def open_meteo_case(m: H.Magician, timeout: int) -> Case:
    case = Case("p2", "open-meteo", "https://open-meteo.com")
    try:
        expected = open_meteo_elevation(*BERLIN)
    except Exception as e:
        case.error = f"oracle unreachable: {e}"
        return case
    title = "Elevation Open-Meteo reports for Berlin"
    desc = describe(
        "Open https://api.open-meteo.com/v1/forecast?latitude=52.52&longitude=13.41"
        "&current=temperature_2m and report the value of the `elevation` field in the JSON the "
        "page shows. Report the number only.")
    case.phases.append(run_phase(m, "cold", title, desc, Expect(answer=[expected]), timeout))
    if not case.phases[0].task_id:
        return case
    recipe = bind_recipe(m, case, case.phases[0].task_id)
    if recipe is None:
        return case
    try:
        warm = open_meteo_elevation(*BERLIN)
    except Exception as e:
        case.error = f"oracle unreachable: {e}"
        return case
    case.phases.append(run_phase(m, "warm", title, desc,
                                 Expect(answer=[warm], outcome_type=RECIPE_REPLAY, browserless=True), timeout))
    return case


RUST_PAGE = "Rust_(programming_language)"


def wikipedia_case(m: H.Magician, timeout: int) -> Case:
    case = Case("p3", "wikipedia", "https://en.wikipedia.org")
    try:
        year = wikipedia_first_appeared_year(RUST_PAGE)
    except Exception as e:
        case.error = f"oracle unreachable: {e}"
        return case
    title = "Year the Rust programming language first appeared"
    desc = describe(
        "Open https://en.wikipedia.org/wiki/Rust_(programming_language) and report the year the "
        "infobox gives on its first-appeared row. Report the year only.")
    case.phases.append(run_phase(m, "cold", title, desc, Expect(answer=[year]), timeout))
    if not case.phases[0].task_id:
        return case
    # The document is server-rendered prose in a large body; mining cannot serve
    # it today. Record whether a recipe bound as a gate, not as a hard failure.
    bound = m.wait_for_recipe_bound(case.phases[0].task_id, 20)
    case.phases[-1].gates.append(Gate(
        "document_answer_read_without_mining", True,
        f"recipe_bound={bound is not None}; a value in prose inside a large server-rendered "
        "document is not minable today (text locator caps the body, needs whole-element text, "
        "and a unique tag)"))
    return case


HN_TITLE_FIELD = "Title of the top Hacker News story about {q}"
HN_BODY_FIELD = ("Open https://hn.algolia.com/?q={q} and report the title of the first story "
                 "result. Report the title only.")


def drift_case(m: H.Magician, timeout: int) -> Case:
    case = Case("p4", "hn.algolia", "https://hn.algolia.com")
    query = "wasm"
    try:
        story_title = hn_top_story_title(query)
    except Exception as e:
        case.error = f"oracle unreachable: {e}"
        return case
    title = HN_TITLE_FIELD.format(q=query)
    desc = describe(HN_BODY_FIELD.format(q=query))
    case.phases.append(run_phase(m, "cold", title, desc, Expect(answer=[story_title]), timeout))
    if not case.phases[0].task_id:
        return case
    recipe = bind_recipe(m, case, case.phases[0].task_id)
    if recipe is None or not case.recipe_id:
        return case
    before = len(recipe.get("versions", []))
    try:
        force_drift(m, case.recipe_id)
    except Exception as e:
        case.error = f"could not force drift: {e}"
        return case

    def drift_gate(obs: Observed):
        failed = H.has_event(obs.events, "recipe.replay.step.failed")
        handed = H.has_event(obs.events, "recipe.replay.fallback.handoff")
        return [Gate("drift_recorded", failed or handed, f"step.failed={failed} handoff={handed}")]

    try:
        drift_title = hn_top_story_title(query)
    except Exception as e:
        case.error = f"oracle unreachable: {e}"
        return case
    case.phases.append(run_phase(m, "drift", title, desc,
                                 Expect(answer=[drift_title], not_outcome_type=RECIPE_REPLAY), timeout,
                                 extra=drift_gate))
    # wait for recompile to a new version
    healed = None
    started = time.time()
    while time.time() - started < RECIPE_WAIT:
        detail = m.get(f"{H.V2}/api-mining/recipes/{case.recipe_id}")
        if len(detail.get("versions", [])) > before:
            healed = detail
            break
        time.sleep(2)
    case.phases[-1].gates.append(Gate("recompiled_new_version", healed is not None,
                                       f"versions {before} -> {len(healed.get('versions', [])) if healed else before}"))
    if healed is None:
        return case
    try:
        healed_title = hn_top_story_title(query)
    except Exception as e:
        case.error = f"oracle unreachable: {e}"
        return case
    case.phases.append(run_phase(m, "healed", title, desc,
                                 Expect(answer=[healed_title], outcome_type=RECIPE_REPLAY, browserless=True), timeout))
    return case


GMAIL_ORIGIN = "https://mail.google.com"


def gmail_case(m: H.Magician, timeout: int) -> Case:
    case = Case("p5", "gmail", GMAIL_ORIGIN)
    title = "Subject of the newest message in the Gmail inbox"
    desc = describe(
        "Open https://mail.google.com/mail/u/0/#inbox with the browser tool, passing "
        'connection_mode "cdp" so it attaches to the already signed-in Chrome. Read the inbox '
        "list and find the subject line of the newest message. Your final summary must be that "
        "subject line and nothing else: do not describe how you read it, which tool or "
        "connection mode you used, or what steps you took. This is strictly read-only.")
    case.phases.append(run_phase(m, "cold", title, desc, Expect(), timeout))
    if not case.phases[0].task_id:
        return case
    recipe = bind_recipe(m, case, case.phases[0].task_id)
    if recipe is None:
        return case
    case.phases.append(run_phase(m, "warm_ungranted", title, desc,
                                 Expect(not_outcome_type=RECIPE_REPLAY), timeout))
    try:
        m.allow_origin_replay(GMAIL_ORIGIN)
    except Exception as e:
        case.error = f"could not grant replay for {GMAIL_ORIGIN}: {e}"
        return case
    reported = case.phases[0].summary_excerpt.strip()
    answer = [reported] if reported and len(reported) <= 120 else []
    case.phases.append(run_phase(m, "warm_granted", title, desc,
                                 Expect(answer=answer, outcome_type=RECIPE_REPLAY, browserless=True), timeout))
    return case


# The operator's real Keka tenant, e.g. https://<company>.keka.com. Set
# MAGICIAN_KEKA_ORIGIN for live runs; the default is a placeholder.
KEKA_ORIGIN = os.environ.get("MAGICIAN_KEKA_ORIGIN", "https://example.keka.com").rstrip("/")


def keka_case(m: H.Magician, timeout: int) -> Case:
    case = Case("p6", "keka", KEKA_ORIGIN)
    title = "Clock out of Keka web attendance"
    desc = describe_write(
        f"On {KEKA_ORIGIN}/#/home/dashboard there is a single attendance clock "
        'button. If it reads "Web Clock-out" or "Clock-out", click it once to clock out. Keka\'s '
        "clock-out is a TWO-STEP action: the first click reveals a confirmation with a second "
        '"Clock-out" button and a "Cancel" — you must then click that second "Clock-out" to '
        "actually clock out. A plain button click via a ref may not register on this control; "
        "if the state does not change, use browser__eval to call .click() on the button element "
        'directly. If it already reads "Web Clock-In", the account is already clocked out — do '
        "not click, and report that. After acting, report only the resulting button label.")
    case.phases.append(run_phase(m, "cold", title, desc, Expect(), timeout))
    if not case.phases[0].task_id:
        return case
    recipe = bind_recipe(m, case, case.phases[0].task_id)
    if recipe is None:
        return case
    try:
        m.allow_origin_replay(KEKA_ORIGIN)
    except Exception as e:
        case.error = f"could not grant replay for {KEKA_ORIGIN}: {e}"
        return case
    case.phases.append(run_phase(m, "warm_granted", title, desc,
                                 Expect(outcome_type=RECIPE_REPLAY, browserless=True), timeout))
    return case


CASES = {
    "p1": hn_case, "p2": open_meteo_case, "p3": wikipedia_case,
    "p4": drift_case, "p5": gmail_case, "p6": keka_case,
}


def run_case(case_id: str, m: H.Magician, timeout: int) -> Case:
    fn = CASES.get(case_id)
    if fn is None:
        c = Case(case_id, "", "")
        c.error = f"unknown case {case_id}"
        return c
    return fn(m, timeout).finish()
