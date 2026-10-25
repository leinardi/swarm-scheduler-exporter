#!/usr/bin/env bash
#
# MIT License
#
# Copyright (c) 2026 Roberto Leinardi
#
# Permission is hereby granted, free of charge, to any person obtaining a copy
# of this software and associated documentation files (the "Software"), to deal
# in the Software without restriction, including without limitation the rights
# to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
# copies of the Software, and to permit persons to whom the Software is
# furnished to do so, subject to the following conditions:
#
# The above copyright notice and this permission notice shall be included in all
# copies or substantial portions of the Software.
#
# THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
# IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
# FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
# AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
# LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
# OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
# SOFTWARE.
#

# rustfmt-on-edit.sh — Claude Code PostToolUse hook (configured in .claude/settings.json).
# Auto-formats Rust files immediately after an Edit/Write tool call so formatting
# drift is fixed at edit time instead of failing later at pre-commit.
#
# Input: hook payload JSON on stdin; the edited path is at .tool_input.file_path.
# rustfmt finds the repository's rustfmt.toml (edition, max_width) from the file's directory.
# Always exits 0 (a formatting fix must never block the agent).

set -euo pipefail

file="$(python3 -c 'import json,sys; print(json.load(sys.stdin).get("tool_input",{}).get("file_path",""))' 2>/dev/null || true)"

case "$file" in
  *.rs)
    if [[ -f "$file" ]] && command -v rustfmt >/dev/null 2>&1; then
      rustfmt --quiet "$file" || true
    fi
    ;;
esac

exit 0
