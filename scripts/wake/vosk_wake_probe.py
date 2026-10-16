#!/usr/bin/env python3
"""Can this assistant name be armed as an on-device wake phrase?

The Vosk spotter is grammar-constrained: it silently drops any word missing from
the model's vocabulary, so a coined name (e.g. "Magican") never fires however it
is pronounced -- with nothing but a log line to say so. This answers, offline:

  1. LEXICON   -- does the word survive grammar compilation at all?
  2. TRUE-ACCEPT  -- does synthesised speech of the name fire the phrase?
  3. FALSE-ACCEPT -- does ordinary speech fire it by accident?

When (1) fails, name in-lexicon stand-ins in the agent definition's
`wake_spellings` and re-run with --spelling to score each candidate.

Usage
-----
  uv run --with vosk scripts/wake/vosk_wake_probe.py --name magican \\
      --spelling magical --spelling magician

macOS only (uses `say` for synthesis). Numbers are directional: synthetic
voices are not the measurement protocol behind VoskWakeSpotter's table.
"""
import argparse, glob, json, os, shutil, subprocess, sys, tempfile, wave

DEFAULT_MODEL = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                             "..", "..", "magios", "Magios", "Resources", "vosk-model")
VOICES = ["Alex", "Samantha", "Daniel"]

# Ordinary speech, no wake intent. Half deliberately contain wake-adjacent words;
# several start with "hey" so the prefix itself is exercised as a near miss.
NEAR_MISS = [
    "that was a magical evening", "the show was absolutely magical",
    "it has a magical quality to it", "magical thinking is a cognitive bias",
    "a magical mystery tour", "she is a magician at spreadsheets",
    "the magician pulled a rabbit out", "hey can you hear me",
    "hey there how are you", "hey what time is the meeting",
    "may I ask you a question", "make it a little bigger",
    "let me check my calendar tomorrow", "I need to buy groceries later",
    "the weather looks nice today",
]


def synth(text, path, voice):
    subprocess.run(["say", "-v", voice, "-o", path, "--data-format=LEI16@16000", text],
                   check=True, capture_output=True)


def transcribe(model, grammar, wav):
    from vosk import KaldiRecognizer
    wf = wave.open(wav, "rb")
    rec = KaldiRecognizer(model, wf.getframerate(), grammar)
    out = []
    while True:
        d = wf.readframes(4000)
        if not d:
            break
        if rec.AcceptWaveform(d):
            out.append(json.loads(rec.Result()).get("text", ""))
    out.append(json.loads(rec.FinalResult()).get("text", ""))
    wf.close()
    return " ".join(t for t in out if t).strip()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--name", required=True, help="assistant name to test, e.g. magican")
    ap.add_argument("--spelling", action="append", default=[],
                    help="candidate in-lexicon stand-in; repeatable")
    ap.add_argument("--model", default=os.path.normpath(DEFAULT_MODEL))
    ap.add_argument("--prefix", default="hey")
    args = ap.parse_args()

    if not shutil.which("say"):
        sys.exit("`say` not found -- this probe is macOS only.")
    if not os.path.isdir(args.model):
        sys.exit("model not found: %s (run `make setup-magios-vosk`)" % args.model)

    from vosk import Model, KaldiRecognizer, SetLogLevel

    # --- 1. LEXICON -------------------------------------------------------
    # An out-of-vocabulary word is dropped, collapsing the grammar to the same
    # shape a nonsense control produces. Compare against that control rather
    # than hardcoding an arc count, so this survives a model upgrade.
    SetLogLevel(-1)
    model = Model(args.model)

    def is_oov(word):
        # Kaldi logs from C, straight to file descriptor 2. `redirect_stderr`
        # only rebinds Python's `sys.stderr` and would capture nothing, silently
        # reporting every word as in-vocabulary -- so dup2 the real fd instead.
        fd, tmp = tempfile.mkstemp()
        os.close(fd)
        saved, sink = os.dup(2), os.open(tmp, os.O_WRONLY)
        try:
            os.dup2(sink, 2)
            SetLogLevel(0)
            KaldiRecognizer(model, 16000, json.dumps(["%s %s" % (args.prefix, word), "[unk]"]))
        finally:
            SetLogLevel(-1)
            os.dup2(saved, 2)
            os.close(saved)
            os.close(sink)
        with open(tmp, errors="replace") as fh:
            text = fh.read()
        os.unlink(tmp)
        return "missing in vocabulary: '%s'" % word.lower() in text.lower()

    print("=== 1. LEXICON ===")
    candidates = [args.name] + args.spelling
    oov = {}
    for w in candidates:
        oov[w] = is_oov(w)
        print("  %-14s %s" % (w, "NOT IN VOCABULARY - can never arm" if oov[w] else "in vocabulary"))

    armable = [w for w in candidates if not oov[w]]
    if not armable:
        print("\nNothing here can be armed. Supply --spelling candidates.")
        return 1

    # --- 2 & 3. ACCEPT RATES ---------------------------------------------
    tmpd = tempfile.mkdtemp(prefix="wakeprobe-")
    true_set, near_set = [], []
    for v in VOICES:
        p = os.path.join(tmpd, "true_%s.wav" % v)
        synth("%s %s" % (args.prefix, args.name), p, v)
        true_set.append(p)
        for i, phrase in enumerate(NEAR_MISS):
            p = os.path.join(tmpd, "near_%02d_%s.wav" % (i, v))
            synth(phrase, p, v)
            near_set.append(p)

    print("\n=== 2/3. ACCEPT RATES (true-accept spoken as %r) ===" % ("%s %s" % (args.prefix, args.name)))
    print("  %-18s | %-16s | %s" % ("armed phrase", "TRUE-ACCEPT", "FALSE-ACCEPT"))
    print("  " + "-" * 60)
    for w in armable:
        phrase = "%s %s" % (args.prefix, w)
        gj = json.dumps([phrase, "[unk]"])
        ta = sum(phrase in transcribe(model, gj, f) for f in true_set)
        fa = sum(phrase in transcribe(model, gj, f) for f in near_set)
        print("  %-18s | %2d/%-2d (%3d%%)    | %2d/%-2d (%3d%%)" % (
            phrase, ta, len(true_set), 100 * ta // len(true_set),
            fa, len(near_set), 100 * fa // len(near_set)))
    shutil.rmtree(tmpd, ignore_errors=True)
    print("\nPick the highest true-accept with a 0% false-accept, and put it in the")
    print("agent definition's `wake_spellings`. Prefer prefixed forms: a bare")
    print("common word false-accepts on ordinary speech.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
