#!/bin/sh
# Read-only tool/public certificate metadata. Never exports or opens private keys,
# checks credentials, changes access rules, signs files, or contacts a service.
set -eu
python3 - <<'PY'
import platform
import shutil
import subprocess


def available(name):
    return shutil.which(name) is not None


def report(label, value):
    print(f"{label}: {value}")


def run(command):
    try:
        result = subprocess.run(command, capture_output=True, text=True, timeout=10)
        return result.stdout if result.returncode == 0 else None
    except (OSError, subprocess.TimeoutExpired):
        return None


system = platform.system()
report("Host", system)
if system == "Darwin":
    report("codesign", "available" if available("codesign") else "missing")
    for name in ("notarytool", "stapler"):
        result = run(["xcrun", "--find", name])
        report(name, "available" if result else "missing")
    # Capture and reduce in memory. Never print names, hashes, team IDs, or paths.
    identities = run(["/usr/bin/security", "find-identity", "-v", "-p", "codesigning"])
    if identities is None:
        report("Developer ID Application identity", "could not inspect")
    else:
        count = sum('"Developer ID Application:' in line for line in identities.splitlines())
        report("Valid Developer ID Application identities", count)
    report("Notarization credentials", "not inspected; owner must provide an authorized signing setup")
    report("Windows Authenticode identity", "not inspected on macOS")
else:
    report("signtool", "available" if available("signtool") or available("signtool.exe") else "not found")
    report("osslsigncode", "available" if available("osslsigncode") else "not found")
    report("Signing certificates", "not inspected on this platform")
report("Changes made", "none")
PY
