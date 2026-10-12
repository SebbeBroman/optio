#!/usr/bin/env python3
"""Native-HiGHS oracle + independent checker for `optio`.

Why: the `highs-js` wasm build used by `src/oracle.mjs` mis-solves MIPs (it
returns "Optimal" for feasible-but-suboptimal points; e.g. planning-s 1918 vs a
verified-feasible 672). This tool uses **native** HiGHS (`highspy`) instead, and
drives `optio-bench` from the model *native HiGHS parsed* — so no wasm-HiGHS is
in the loop for either the reference or the model.

For each `.lp` it:
  1. parses + solves with native HiGHS (time-limited) -> reference status,
     objective, MIP gap and dual bound;
  2. re-emits the parsed model as the CSR JSON `optio-bench` consumes;
  3. runs `optio-bench --model-json` (optionally `--lp`);
  4. independently re-checks optio's solution (rows/bounds/integrality) in pure
     Python and compares objective vs the native reference.

Setup:  bash harness/tools/setup-oracle.sh   (creates a venv with highspy)
Run:    harness/tools/oracle-venv/bin/python harness/tools/native-oracle.py \\
            harness/fixtures/planning-s.lp --time 20
"""
import argparse
import json
import math
import os
import subprocess
import sys
import tempfile
import time

try:
    import highspy
except ImportError:
    sys.exit("highspy not found; run `bash harness/tools/setup-oracle.sh`")

REPO = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
BENCH = os.path.join(REPO, "target", "release", "optio-bench")


def integrality_code(t):
    s = str(t)
    if "Continuous" in s:
        return 0
    return 1  # integer / binary-like (bounds decide binary in the solver)


def csc_to_csr(start, index, value, n, m):
    """CSC (col-major) -> CSR (row-major) parallel arrays."""
    counts = [0] * (m + 1)
    for c in range(n):
        for k in range(start[c], start[c + 1]):
            counts[index[k] + 1] += 1
    for r in range(m):
        counts[r + 1] += counts[r]
    cursor = counts[:]
    cidx = [0] * len(index)
    cval = [0.0] * len(value)
    for c in range(n):
        for k in range(start[c], start[c + 1]):
            r = index[k]
            p = cursor[r]
            cidx[p] = c
            cval[p] = value[k]
            cursor[r] += 1
    return counts, cidx, cval


def _bnd(v):
    """+/-inf -> the finite sentinel the CLI uses."""
    if v == float("inf"):
        return 1e30
    if v == float("-inf"):
        return -1e30
    return float(v)


def model_json(lp):
    n, m = lp.num_col_, lp.num_row_
    a = lp.a_matrix_
    start = [int(x) for x in a.start_]
    index = [int(x) for x in a.index_]
    value = [float(x) for x in a.value_]
    starts, indices, values = csc_to_csr(start, index, value, n, m)
    integ = [integrality_code(t) for t in lp.integrality_] if len(lp.integrality_) else [0] * n
    return {
        "numCols": n,
        "numRows": m,
        "sense": (-1 if "Maximize" in str(lp.sense_) else 1),
        "offset": float(lp.offset_),
        "colCost": [float(x) for x in lp.col_cost_],
        "colLower": [_bnd(x) for x in lp.col_lower_],
        "colUpper": [_bnd(x) for x in lp.col_upper_],
        "rowLower": [_bnd(x) for x in lp.row_lower_],
        "rowUpper": [_bnd(x) for x in lp.row_upper_],
        "integrality": integ,
        "matrix": {"format": "csr", "starts": starts, "indices": indices, "values": values},
    }


def check(model, x, tol=1e-6):
    n, m = model["numCols"], model["numRows"]
    if x is None or len(x) < n:
        return {"feasible": None, "error": f"short solution ({0 if not x else len(x)} < {n})"}
    maxb = maxi = 0.0
    for c in range(n):
        maxb = max(maxb, model["colLower"][c] - x[c], x[c] - model["colUpper"][c])
        if model["integrality"][c] != 0:
            maxi = max(maxi, abs(x[c] - round(x[c])))
    mat = model["matrix"]
    s, idx, val = mat["starts"], mat["indices"], mat["values"]
    maxr = 0.0
    for r in range(m):
        lhs = 0.0
        for k in range(s[r], s[r + 1]):
            lhs += val[k] * x[idx[k]]
        maxr = max(maxr, model["rowLower"][r] - lhs, lhs - model["rowUpper"][r])
    obj = sum(model["colCost"][c] * x[c] for c in range(n)) + model.get("offset", 0.0)
    return {
        "feasible": maxr <= tol and maxb <= tol and maxi <= tol,
        "objective": obj,
        "maxRowViolation": maxr,
        "maxBoundViolation": maxb,
        "maxIntegralityViolation": maxi,
    }


def solve_reference(path, seconds, relative_gap=None):
    h = highspy.Highs()
    h.setOptionValue("output_flag", False)
    h.setOptionValue("time_limit", float(seconds))
    if relative_gap is not None:
        h.setOptionValue("mip_rel_gap", float(relative_gap))
        h.setOptionValue("mip_abs_gap", 0.0)
    h.readModel(path)
    t = time.time()
    h.run()
    dt = time.time() - t
    status = str(h.getModelStatus())
    obj = None
    gap = None
    dual = None
    try:
        info = h.getInfo()
        gap = info.mip_gap
        dual = info.mip_dual_bound
    except Exception:
        pass
    # objective is only meaningful when a solution exists
    names_with_sol = ("kOptimal", "kTimeLimit", "kIterationLimit", "kObjectiveBound",
                      "kObjectiveTarget", "kSolutionLimit", "kInterrupted")
    if any(n in status for n in names_with_sol) and h.getSolution().value_valid:
        obj = h.getObjectiveValue()
    obj = obj if obj is not None and math.isfinite(obj) else None
    gap = gap if gap is not None and math.isfinite(gap) else None
    dual = dual if dual is not None and math.isfinite(dual) else None
    return h, status, obj, gap, dual, dt


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("lps", nargs="+")
    ap.add_argument("--time", type=float, default=30.0, help="reference time limit (s)")
    ap.add_argument("--ref-gap", type=float, default=None, help="HiGHS relative gap (default: its own default)")
    ap.add_argument("--optio-time", type=float, default=0.0, help="optio time limit (s); 0=use --time")
    ap.add_argument("--lp", action="store_true", help="compare optio's LP relaxation")
    ap.add_argument("--ref-only", action="store_true",
                    help="only solve the native HiGHS reference (skip optio-bench)")
    ap.add_argument("--json-out", default="", help="write per-instance records here")
    args = ap.parse_args()

    if not os.path.exists(BENCH):
        sys.exit(f"optio-bench not found at {BENCH}; run `cargo build --release -p optio-bench`")

    pad = lambda s, n: str(s).ljust(n)
    print(f"native HiGHS {highspy.Highs().version()} | optio {BENCH}")
    print(pad("fixture", 24) + pad("ref status", 16) + pad("ref obj", 14) +
          pad("gap", 8) + pad("dual", 12) + pad("optio", 12) + pad("optio obj", 14) +
          pad("rel", 10) + "optio feas")
    print("-" * 120)

    records = []
    for path in args.lps:
        name = os.path.basename(path)
        h, status, refobj, gap, dual, dt = solve_reference(path, args.time, args.ref_gap)
        model = model_json(h.getLp())

        if args.ref_only:
            fmt = lambda v, d=3: f"{v:.{d}f}" if isinstance(v, (int, float)) else str(v)
            print(pad(name, 24) + pad(status.split(".")[-1], 16) + pad(fmt(refobj), 14) +
                  pad(fmt(gap, 4) if gap is not None else "-", 8) +
                  pad(fmt(dual) if dual is not None else "-", 12) +
                  pad("-", 12) + pad("-", 14) + pad("-", 10) + "-")
            records.append({"file": path, "reference": {"status": status, "objective": refobj,
                            "gap": gap, "dual_bound": dual, "seconds": dt}})
            continue

        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            json.dump(model, f)
            tmp = f.name
        ot = args.optio_time if args.optio_time > 0 else args.time
        cmd = [BENCH, "--model-json", tmp, "--time", str(ot)]
        if args.lp:
            cmd.append("--lp")
        try:
            out = subprocess.run(cmd, capture_output=True, text=True, timeout=ot + 60)
            rec = json.loads(out.stdout.strip().splitlines()[-1])
        except Exception as e:
            rec = {"error": str(e)}
        finally:
            os.unlink(tmp)

        chk = None
        if "colValue" in rec and rec.get("colValue"):
            chk = check(model, rec["colValue"], 1e-6)
        oobj = rec.get("objective")
        rel = None
        if isinstance(oobj, (int, float)) and isinstance(refobj, (int, float)) and refobj not in (0, None):
            rel = abs(oobj - refobj) / max(1.0, abs(refobj))

        def num(v, d=3):
            return f"{v:.{d}f}" if isinstance(v, (int, float)) else str(v)

        print(pad(name, 24) + pad(status.split(".")[-1], 16) + pad(num(refobj), 14) +
              pad(num(gap, 4) if gap is not None else "-", 8) +
              pad(num(dual) if dual is not None else "-", 12) +
              pad(f"{rec.get('statusCode', rec.get('error','?'))}", 12) +
              pad(num(oobj), 14) + pad(num(rel, 4) if rel is not None else "-", 10) +
              (("yes" if chk["feasible"] else "NO") if chk else "n/a"))
        records.append({"file": path, "reference": {"status": status, "objective": refobj,
                        "gap": gap, "dual_bound": dual, "seconds": dt}, "optio": rec, "check": chk})

    if args.json_out:
        with open(args.json_out, "w") as f:
            json.dump(records, f, indent=2)
        print(f"\nwrote {args.json_out}")


if __name__ == "__main__":
    main()
