#!/usr/bin/env python3
"""FPL-02 live development set: four long tasks over three turns each, run
through coding sessions (as in the app). The finding the last turn needs is
in a long tool output of the first turn – not at its start. Deterministic;
writes long-tasks-coding.yaml next to this file."""
import json, pathlib

def yaml_str(s):  # JSON strings are valid YAML scalars
    return json.dumps(s, ensure_ascii=False)

tasks = []

# T1 – a one-off import: the batch is archived afterwards.
rows = ["customer,amount"]
for i in range(1, 2401):
    cust, amount = f"K-{1000 + (i * 37) % 9000}", f"{(i * 7) % 300 + 0.5:.2f}"
    if i == 917: cust, amount = "K-2291", "\"12,50\""
    if i == 1873: cust, amount = "K-4471", "n/a"
    rows.append(f"{cust},{amount}")
import_py = '''import csv, os, shutil, sys
src = "incoming/orders.csv"
if not os.path.exists(src):
    print("nothing to import: incoming/ is empty (the last batch was archived)")
    sys.exit(0)
rows = list(csv.reader(open(src)))[1:]
bad = 0
for i, (customer, amount) in enumerate(rows, start=1):
    try:
        float(amount)
        print(f"row {i:04d} ok   customer {customer} amount {amount}")
    except ValueError:
        bad += 1
        print(f"row {i:04d} REJECTED: amount {amount!r} is not a number (customer {customer})")
print(f"imported {len(rows) - bad} of {len(rows)} rows")
os.makedirs("archive", exist_ok=True)
shutil.move(src, "archive/orders-batch-17.csv")
'''
tasks.append({"id": "fpl02-import-once", "kind": "long-result",
  "files": {"import.py": import_py, "incoming/orders.csv": "\n".join(rows) + "\n"},
  "turns": ["Run python3 import.py. Tell me only how many rows were imported – nothing else.",
            "Which Python modules does import.py use? One sentence.",
            "Write every rejected row from that import into rejected.txt, one per line as row,customer (for example 0005,K-1000)."],
  "checks": [{"file_contains": {"path": "rejected.txt", "text": "0917,K-2291"}},
             {"file_contains": {"path": "rejected.txt", "text": "1873,K-4471"}}]})

# T2 – a test run, then a fix later (the run can be repeated).
prices = '''VAT = 0.19

def net_to_gross(net):
    return round(net * (1 + VAT * 10), 2)

def discount(price, percent):
    return round(price * (1 - percent / 100), 2)
'''
catalog = '''def find(items, name):
    return [i for i in items if i["name"] == name]
'''
run_tests = '''import sys
from shop.prices import net_to_gross, discount
failed = 0
n = 0
for p in range(1, 301):
    n += 1
    ok = discount(p, 10) == round(p * 0.9, 2)
    print(f"check discount_{p:03d} ... {'ok' if ok else 'FAILED'}")
    failed += not ok
    if p in (120, 121, 122):
        n += 1
        got = net_to_gross(p)
        want = round(p * 1.19, 2)
        if got == want:
            print(f"check net_to_gross_{p} ... ok")
        else:
            failed += 1
            print(f"check net_to_gross_{p} ... FAILED: expected {want}, got {got}")
for p in range(1, 301):
    n += 1
    print(f"check discount_zero_{p:03d} ... ok" if discount(p, 0) == p else f"check discount_zero_{p:03d} ... FAILED")
print(f"{n - failed} passed, {failed} failed")
sys.exit(1 if failed else 0)
'''
tasks.append({"id": "fpl02-tests-then-fix", "kind": "long-result",
  "files": {"shop/__init__.py": "", "shop/prices.py": prices, "shop/catalog.py": catalog, "run_tests.py": run_tests},
  "turns": ["Run python3 run_tests.py and remember the result. Don't change anything yet.",
            "Briefly: what does shop/catalog.py do?",
            "Now fix the bug that the test run found."],
  "checks": [{"command": "python3 run_tests.py"}]})

# T3 – a check that runs once a day: an early "passed", a blocking finding later.
deploy = '''#!/bin/sh
if [ -f .check_ran ]; then
  echo "deploy_check already ran today – it is limited to once a day. Use its earlier output."
  exit 3
fi
touch .check_ran
echo "Stage 1/3: unit tests"
echo "All checks passed (412)"
i=0; while [ $i -lt 900 ]; do echo "  stage 2: verifying asset $i ... ok"; i=$((i+1)); done
echo "  stage 2: verifying certificate api.shop.test ... FAILED: expires in 3 days (2026-10-09)"
i=0; while [ $i -lt 400 ]; do echo "  stage 3: smoke test $i ... ok"; i=$((i+1)); done
echo "Result: 1 blocking problem"
exit 1
'''
tasks.append({"id": "fpl02-deploy-go", "kind": "long-result",
  "files": {"deploy_check.sh": deploy, "config/deploy.yaml": "region: eu-north\nreplicas: 3\n"},
  "turns": ["Run sh deploy_check.sh. Just say whether it finished.",
            "What region does config/deploy.yaml set?",
            "Should we deploy today? Write GO or NO-GO and the reason into decision.txt."],
  "checks": [{"command": "grep -qi 'no-go' decision.txt && grep -qi 'certif' decision.txt"}]})

# T4 – a long listing whose start matters (no loss against before).
misc = "".join(f"value_{i} = {i}  # TODO later: rename value_{i}\n" for i in range(500))
users = '''def save_user(db, name, email):
    # TODO: validate the email address before saving
    db.append({"name": name, "email": email})
'''
tasks.append({"id": "fpl02-todo-listing", "kind": "long-result",
  "files": {"src/a_users.py": users, "src/zz_misc.py": misc},
  "turns": ["Find all TODO comments under src and tell me how many there are.",
            "Which file in src is the largest?",
            "Fix the very first TODO of the list you found."],
  "checks": [{"command": "! grep -q 'TODO: validate the email' src/a_users.py"},
             {"command": "python3 -c \"import ast; ast.parse(open('src/a_users.py').read())\""},
             {"command": "grep -q '@' src/a_users.py"}]})

out = ["# FPL-02 live development set – generated by make-long-tasks.py; do not edit.", "name: fpl02-long-tasks", "tasks:"]
for t in tasks:
    out.append(f"  - id: {t['id']}")
    out.append(f"    kind: {t['kind']}")
    out.append("    files:")
    for k, v in t["files"].items():
        out.append(f"      {yaml_str(k)}: {yaml_str(v)}")
    out.append("    turns:")
    for x in t["turns"]:
        out.append(f"      - {yaml_str(x)}")
    out.append("    checks:")
    for c in t["checks"]:
        out.append(f"      - {json.dumps(c, ensure_ascii=False)}")
p = pathlib.Path(__file__).with_name("long-tasks-coding.yaml")
p.write_text("\n".join(out) + "\n")
print(f"{len(tasks)} tasks → {p}")
