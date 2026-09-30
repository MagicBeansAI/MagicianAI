#!/usr/bin/env python3
# Usage: scripts/laya-onnx-reference.py <laya source checkout> <laya-onnx model dir> \
#            magician-decision/tests/fixtures/laya/cases.json > .../onnx_reference.json
# (needs numpy, onnxruntime, tokenizers; laya's torch import is stubbed)
"""laya's own sequence building and post-processing, run on ONNX Runtime
(Python) — the reference the Rust port must match. torch is stubbed: the
imported functions never touch it."""
import json, sys, types
import numpy as np

torch = types.ModuleType("torch"); nn = types.ModuleType("torch.nn")
nn.Module = object; torch.nn = nn
utils = types.ModuleType("torch.utils"); cp = types.ModuleType("torch.utils.checkpoint"); cp.checkpoint = None
torch.utils = utils; utils.checkpoint = cp
for name, mod in {"torch": torch, "torch.nn": nn, "torch.utils": utils, "torch.utils.checkpoint": cp}.items():
    sys.modules[name] = mod
sys.path.insert(0, sys.argv[1])  # laya source
from laya.common import build_sequence, render_options, confidence_from_probs, temp_bucket, clamp_temperature, QTYPES
import onnxruntime as ort
from tokenizers import Tokenizer

model_dir, cases_path = sys.argv[2], sys.argv[3]
cfg = json.load(open(f"{model_dir}/rl_agent_config.json"))
tcfg = json.load(open(f"{model_dir}/tokenizer_config.json"))
raw = Tokenizer.from_file(f"{model_dir}/tokenizer.json")

class Tok:
    mask_token = tcfg["mask_token"]
    mask_token_id = raw.token_to_id(tcfg["mask_token"])
    cls_token_id = raw.token_to_id(tcfg["cls_token"])
    sep_token_id = raw.token_to_id(tcfg["sep_token"])
    pad_token_id = raw.token_to_id(tcfg["pad_token"])
    def __call__(self, text, add_special_tokens=False):
        return {"input_ids": raw.encode(text, add_special_tokens=add_special_tokens).ids}
tok = Tok()
session = ort.InferenceSession(f"{model_dir}/model.onnx", providers=["CPUExecutionProvider"])
temps = [clamp_temperature(t) for t in cfg.get("temperature", [1, 1, 1])]
tbo = {k: clamp_temperature(v) for k, v in cfg.get("temperature_by_options", {}).items()}

def to_internal(q):
    t, crit = q["type"], q.get("criteria")
    if t == "noul" and isinstance(crit, dict):
        crit = {str(k).lower(): v for k, v in crit.items()}
    if t == "choice":
        crit = {k: (None if v == "" else v) for k, v in crit.items()}
    return {"t": t, "ins": q["instructions"], "crit": crit}

out = {}
for case in json.load(open(cases_path)):
    ids = list(case["questions"])
    qs = [to_internal(case["questions"][i]) for i in ids]
    items = [build_sequence(tok, case["state"], q, cfg["max_len"], cfg["head_max_len"]) for q in qs]
    L = max(len(s) for s, _ in items); K = max(len(m) for _, m in items)
    n = len(items)
    inp = {"input_ids": np.full((n, L), tok.pad_token_id, np.int64), "attention_mask": np.zeros((n, L), np.int64),
           "marker_pos": np.zeros((n, K), np.int64), "marker_mask": np.zeros((n, K), bool),
           "qtype": np.array([QTYPES[q["t"]] for q in qs], np.int64)}
    for r, (s, m) in enumerate(items):
        inp["input_ids"][r, :len(s)] = s; inp["attention_mask"][r, :len(s)] = 1
        inp["marker_pos"][r, :len(m)] = m; inp["marker_mask"][r, :len(m)] = True
    logits = session.run(["logits"], inp)[0]
    answers = {}
    for r, (qid, q) in enumerate(zip(ids, qs)):
        k = len(items[r][1]); qt = QTYPES[q["t"]]
        t = tbo.get(temp_bucket(qt, k), temps[qt])
        z = logits[r, :k] / t; p = np.exp(z - z.max()); p = p / p.sum()
        answers[qid] = {"probabilities": [round(float(v), 4) for v in p],
                        "confidence": round(confidence_from_probs(p, k), 4),
                        "ids": [int(x) for x in items[r][0]]}
    out[case["name"]] = answers
json.dump(out, sys.stdout)
