#!/usr/bin/env python3
# Usage: scripts/laya-mlx-reference.py magician-decision/tests/fixtures/laya/cases_mlx.json \
#            .../mlx_reference_multilingual.json [convaiinnovations/laya-multilingual [revision]]
# (run inside a laya-mlx environment: `pip install laya-mlx`; Apple Silicon)
"""Reference for the Rust laya-mlx port: laya-mlx on the original checkpoint.

For each case/question: laya-mlx's own token rows (ids, markers, qtype),
the raw scorer logits in float32 and float16, and predict()'s answers.
"""
import json
import sys

import numpy as np
import laya_mlx as laya
from laya_mlx.agent import collate_items

cases = json.load(open(sys.argv[1]))
out = {}
# argv[3] / argv[4]: checkpoint and revision (what setup-decision-models pins)
MODEL = sys.argv[3] if len(sys.argv) > 3 else "convaiinnovations/laya-multilingual"
REV = sys.argv[4] if len(sys.argv) > 4 else "052592a15d198d9ad47da779604259b10b47b7aa"
agents = {dt: laya.load(MODEL, dtype=dt, revision=REV) for dt in ("float32", "float16")}
for case in cases:
    name, state, questions = case["name"], case["state"], case["questions"]
    ref = {}
    for dt, agent in agents.items():
        items, _ = agent.prepare(state, questions)
        answers = agent.predict(state, questions)["answers"]
        for (qid, _), item in zip(questions.items(), items):
            batch = collate_items([item], agent.tok.pad_token_id)
            logits, _ = agent.forward(batch)
            k = len(item["markers"])
            entry = ref.setdefault(qid, {"ids": [int(i) for i in item["ids"]], "markers": [int(m) for m in item["markers"]], "qtype": int(item["qtype"])})
            entry[f"logits_{dt}"] = [float(v) for v in np.asarray(logits)[0, :k]]
            a = answers[qid]
            probs = list(a["probabilities"].values()) if "probabilities" in a else [round(1 - a["noul"], 4), a["noul"]]
            entry[f"probabilities_{dt}"] = probs
    out[name] = ref
json.dump(out, open(sys.argv[2], "w"), indent=1)
print("cases", len(out), "questions", sum(len(v) for v in out.values()))
