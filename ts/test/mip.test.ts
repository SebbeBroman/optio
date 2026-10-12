import { test } from "node:test";
import assert from "node:assert/strict";
import { Model, VAR_INTEGER } from "../src/model.ts";
import { defaultOptions } from "../src/options.ts";
import { branchAndBound } from "../src/mip.ts";
import { ModelStatus } from "../src/status.ts";
import type { Clock } from "../src/clock.ts";

class Expired implements Clock {
  nowSecs(): number {
    return 1000.0;
  }
}

test("interrupted_lp_cannot_be_reported_as_proven_optimal", () => {
  const m = new Model();
  const a = m.addCol(1, 1);
  const b = m.addCol(0, 1);
  const c = m.addCol(0, 1);
  for (const j of [a, b, c]) m.setIntegrality(j, VAR_INTEGER);
  m.setCost(c, -3);
  m.addRow(1, 1, [
    [b, 1],
    [c, 1],
  ]);
  m.addRow(1, 1, [
    [a, 1],
    [b, 1],
  ]);
  const opts = defaultOptions();
  opts.threads = 2;
  opts.timeLimitSecs = -1;
  const s = branchAndBound(m, opts, new Expired(), {}, null, null);
  assert.notEqual(s.status, ModelStatus.Optimal);
  assert.notEqual(s.status, ModelStatus.Infeasible);
});
