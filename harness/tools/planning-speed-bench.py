#!/usr/bin/env python3
"""Alternate frozen/current builds in fresh processes; verify every optimum."""
import argparse
import hashlib
import json
import pathlib
import statistics
import subprocess

ROOT = pathlib.Path(__file__).resolve().parents[2]
p = argparse.ArgumentParser()
p.add_argument('--before', type=pathlib.Path, required=True)
p.add_argument('--after', type=pathlib.Path, required=True)
p.add_argument('--out', type=pathlib.Path, required=True)
p.add_argument('--repeats', type=int, default=7)
args = p.parse_args()
assert args.repeats > 0
args.out.mkdir(parents=True, exist_ok=True)

def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

records = []
summary = []
for runtime in ['wasm', 'native']:
    for case, optimum in [('small', 314.9), ('medium', 3096.8), ('large', 4028.4)]:
        model = ROOT / f'harness/fixtures/planning-app-{case}.model.json'
        seed = model.with_name(f'planning-app-{case}.start.json')
        configurations = [True, False] if runtime == 'wasm' else [1, 8]
        for config in configurations:
            samples = {'before': [], 'after': []}
            phases = {'before': {}, 'after': {}}
            for repeat in range(args.repeats):
                # Alternate execution order to reduce drift/thermal bias.
                order = ['before', 'after'] if repeat % 2 == 0 else ['after', 'before']
                for tag in order:
                    build = getattr(args, tag).resolve()
                    if runtime == 'wasm':
                        binary = build / 'pkg/optio_wasm_bg.wasm'
                        cmd = ['node', str(ROOT / 'harness/tools/planning-timing.mjs'),
                               str(build / 'pkg/optio_wasm.js'), str(model), '5']
                        if config:
                            cmd += ['--start', str(seed)]
                    else:
                        binary = build / 'native'
                        cmd = [str(binary), '--model-json', str(model), '--time', '5',
                               '--gap', '0', '--threads', str(config)]
                    result = json.loads(subprocess.check_output(cmd, cwd=ROOT, text=True))
                    assert result['statusCode'] == 7, (runtime, case, tag, result['statusCode'])
                    assert abs(result['objective'] - optimum) < 1e-7
                    assert result['dualBound'] <= result['objective']
                    assert result['objective'] - result['dualBound'] < 1e-6
                    assert result['nodes'] == 0
                    if runtime == 'wasm':
                        assert result['check']['feasible']
                        assert max(result['check'][k] for k in ['maxRowViolation',
                                   'maxBoundViolation', 'maxIntegralityViolation']) <= 1e-9
                    else:
                        assert max(result[k] for k in ['maxRowViolation',
                                   'maxBoundViolation', 'maxIntegralityViolation']) <= 1e-9
                    samples[tag].append(result['ms'])
                    for phase, seconds in result['timing'].items():
                        phases[tag].setdefault(phase, []).append(seconds * 1000)
                    # Keep one witness per configuration/build; retain all checks/timings.
                    if repeat:
                        result.pop('colValue')
                    records.append({'runtime': runtime, 'case': case, 'config': config,
                                    'repeat': repeat, 'build': tag, 'buildSha256': sha(binary),
                                    'modelSha256': sha(model), 'seedSha256': sha(seed),
                                    'result': result})
            item = {'runtime': runtime, 'case': case, 'config': config,
                    'objective': optimum, 'samplesMs': samples,
                    'medianMs': {tag: statistics.median(v) for tag, v in samples.items()},
                    'medianPhaseMs': {tag: {k: statistics.median(v) for k, v in vals.items()}
                                      for tag, vals in phases.items()}}
            item['speedup'] = item['medianMs']['before'] / item['medianMs']['after']
            summary.append(item)
            print(runtime, case, config, item['medianMs'], round(item['speedup'], 2), flush=True)
            (args.out / 'repeated-results.json').write_text(json.dumps(records, indent=2) + '\n')
            (args.out / 'repeated-summary.json').write_text(json.dumps(summary, indent=2) + '\n')
