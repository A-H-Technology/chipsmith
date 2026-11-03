#!/usr/bin/env python3
"""Vivado's routing delays, connection by connection, fitted per node class.

    bench/zybo-timing-fit.py [--test <dir> ...] <design dir> ...

Each <design dir> is one of bench/zybo-timing.sh's, holding Vivado's
connections.tsv (xc7-re tools/import.sh: per routed net its ROUTE and each
load's site pin node with Vivado's slow-max delay to it). Every
connection's path through the ROUTE tree is its nodes; each node has a
class (the wire it is named after, digits and tile side dropped: `EE4BEG`,
`IMUX`; a slice pin as `L:A1` or `M:A1`, SLICEL or SLICEM). Fits each
class's delay by non-negative least squares, plus a term per extra branch
the path's nodes drive (the tree's load), and prints the classes (count,
fitted ps, prjxray-db's intrinsic delay for the pip into it or the site
pin) and the fit's error per connection on the fitting designs and on the
--test ones.

Needs numpy and scipy (bench/zybo-timing.sh's devshell has neither:
`nix shell --impure --expr 'with import <nixpkgs> {}; python3.withPackages
(p: [p.numpy p.scipy])'`).
"""
import json
import os
import re
import statistics
import sys
from collections import Counter, defaultdict

import numpy as np
from scipy.optimize import lsq_linear

SITE_PIN = re.compile(r"^CLBL[LM]_(L|LL|M)_([A-D](?:[1-6]|X|I|MUX|Q|MUX)?|[A-D]5Q|CE|SR|CLK|CIN|COUT|WE)$")


def node_class(node):
    tile, wire = node.split("/", 1)
    m = SITE_PIN.match(wire)
    if m:
        side = "M" if m[1] == "M" else "L"
        return f"{side}:{m[2]}"
    w = re.sub(r"\d+$", "", wire)
    w = re.sub(r"_L$", "", w)
    w = w.replace("_L_", "_").replace("LOGIC_OUTS_L", "LOGIC_OUTS")
    if w.startswith("CLBLM_") or w.startswith("CLBLL_"):
        w = "CLB_" + w.split("_", 2)[2] if w.count("_") >= 2 else w
    return w


def parse_route(route):
    """ROUTE string -> (root, parent map, children count)."""
    toks = route.replace("{", " { ").replace("}", " } ").split()
    parent, stack, cur, root = {}, [], None, None
    kids = Counter()
    for t in toks:
        if t == "{":
            stack.append(cur)
        elif t == "}":
            cur = stack.pop()
        elif t not in parent:
            parent[t] = cur
            if cur is None:
                root = root or t
            else:
                kids[cur] += 1
            cur = t
    return root, parent, kids


def connections(d):
    out = []
    for line in open(os.path.join(d, "connections.tsv")):
        f = line.rstrip("\n").split("\t")
        if len(f) < 3 or not f[2]:
            continue
        root, parent, kids = parse_route(f[1])
        for load in f[2].split():
            node, delay = load.rsplit("=", 1)
            if node not in parent:
                continue
            path, n = [], node
            while n is not None and len(path) < 1000:
                path.append(n)
                n = parent.get(n)
            path.reverse()
            out.append((path, kids, float(delay)))
    return out


def prjxray_classes():
    """Intrinsic slow-max delay (ps) per class, from prjxray-db."""
    db = os.environ.get("CHIPSMITH_PRJXRAY_DB")
    if not db:
        return {}
    by = defaultdict(list)
    for tt in ["INT_L", "INT_R"]:
        t = json.load(open(f"{db}/zynq7/tile_type_{tt}.json"))
        for p in t["pips"].values():
            by[node_class(f"{tt}/{p['dst_wire']}")].append(1000 * float(p["src_to_dst"]["delay"][3]))
    for tt in ["CLBLL_L", "CLBLM_L"]:
        t = json.load(open(f"{db}/zynq7/tile_type_{tt}.json"))
        for s in t["sites"]:
            for pin, v in s["site_pins"].items():
                if "delay" in v:
                    by[node_class(f"{tt}/{v['wire']}")].append(1000 * float(v["delay"][3]))
    return {k: statistics.mean(v) for k, v in by.items()}


def features(conns, classes):
    index = {c: i for i, c in enumerate(classes)}
    x = np.zeros((len(conns), len(classes) + 1))
    y = np.zeros(len(conns))
    for r, (path, kids, delay) in enumerate(conns):
        for n in path[1:]:
            c = node_class(n)
            if c in index:
                x[r, index[c]] += 1
        x[r, -1] = sum(max(0, kids[n] - 1) for n in path[:-1])
        y[r] = delay
    return x, y


def report(name, pred, y):
    err = np.abs(pred - y) / np.maximum(y, 1)
    print(f"  {name}: {len(y)} connections, |error| median {100 * np.median(err):.1f}% "
          f"p95 {100 * np.quantile(err, 0.95):.1f}%, signed median "
          f"{100 * np.median((pred - y) / np.maximum(y, 1)):+.1f}%")


args = sys.argv[1:]
test = []
if args and args[0] == "--test":
    args = args[1:]
    while args and args[0] != "--":
        test.append(args.pop(0))
    args = args[1:]
fit = [c for d in args for c in connections(d)]
held = [c for d in test for c in connections(d)]
counts = Counter(node_class(n) for p, _, _ in fit for n in p[1:])
classes = sorted(c for c, k in counts.items() if k >= 3)
x, y = features(fit, classes)
w = lsq_linear(x, y, bounds=(0, np.inf)).x
px = prjxray_classes()
print(f"{'class':<16} {'n':>6} {'fit ps':>7} {'prjxray':>8}")
for c, v in sorted(zip(classes, w), key=lambda cv: -counts[cv[0]]):
    print(f"{c:<16} {counts[c]:>6} {v:7.0f} {px.get(c, float('nan')):8.0f}")
print(f"{'branch':<16} {'':>6} {w[-1]:7.1f}")
report("fit, fitted designs", x @ w, y)
pv = np.array([px.get(c, 0.0) for c in classes] + [0.0])
report("prjxray-db intrinsic, fitted designs", x @ pv, y)
if held:
    xh, yh = features(held, classes)
    report("fit, test designs", xh @ w, yh)
    report("prjxray-db intrinsic, test designs", xh @ pv, yh)
# The table chipsmith reads (crates/chipsmith-xc7/data/), to $FIT_OUT.
out = os.environ.get("FIT_OUT")
if out:
    with open(out, "w") as f:
        f.write("# Routing delay (ps, slow max) per node class, fitted by bench/zybo-timing-fit.py\n")
        f.write(f"# to Vivado's net delays on {len(y)} connections of: {' '.join(os.path.basename(a) for a in args)}\n")
        for c, v in zip(classes, w):
            f.write(f"{c}\t{v:.0f}\t{counts[c]}\n")
