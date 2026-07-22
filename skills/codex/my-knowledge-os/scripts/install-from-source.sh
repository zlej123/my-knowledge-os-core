#!/usr/bin/env bash
set -euo pipefail

repo_root=""
codex_home="${CODEX_HOME:-${HOME}/.codex}"
plan_only=0
assume_yes=0
skip_skill=0

usage() {
  cat <<'EOF'
Usage: install-from-source.sh [--repo PATH] [--codex-home PATH] [--plan] [--yes] [--skip-skill]

Builds mko from an already cloned repository and installs the canonical Codex skill.
It does not run mko setup or modify a knowledge repository.
EOF
}

while (($#)); do
  case "$1" in
    --repo) repo_root="${2:?--repo requires a path}"; shift 2 ;;
    --codex-home) codex_home="${2:?--codex-home requires a path}"; shift 2 ;;
    --plan) plan_only=1; shift ;;
    --yes) assume_yes=1; shift ;;
    --skip-skill) skip_skill=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
if [[ -z "$repo_root" ]]; then
  repo_root="$(cd "$script_dir/../../../.." && pwd -P)"
else
  repo_root="$(cd "$repo_root" && pwd -P)"
fi

cargo_manifest="$repo_root/rust/mko-cli/Cargo.toml"
skill_source="$repo_root/skills/codex/my-knowledge-os"
if [[ ! -f "$cargo_manifest" ]]; then
  echo "mko CLI source was not found at $cargo_manifest. Clone zlej123/my-knowledge-os-core and pass --repo." >&2
  exit 2
fi
if [[ ! -f "$skill_source/SKILL.md" ]]; then
  echo "Canonical My Knowledge OS skill was not found at $skill_source." >&2
  exit 2
fi

cargo_home="${CARGO_HOME:-${HOME}/.cargo}"
mko_binary="$cargo_home/bin/mko"
skill_target="$codex_home/skills/my-knowledge-os"

echo "My Knowledge OS source installation plan"
echo "  repository : $repo_root"
echo "  CLI source : $repo_root/rust/mko-cli"
echo "  CLI target : $mko_binary"
if ((skip_skill)); then
  echo "  Codex skill: skipped"
else
  echo "  Codex skill: $skill_target"
fi
echo "  setup      : not run"

if ! command -v cargo >/dev/null 2>&1; then
  echo >&2
  echo "Rust/Cargo is missing. Install Rust 1.97 or newer from https://rustup.rs/, restart the terminal, and rerun this command." >&2
  exit 2
fi
if ((plan_only)); then
  exit 0
fi

if ((!assume_yes)); then
  if [[ ! -t 0 ]]; then
    echo "Interactive confirmation is unavailable. Review the plan, then rerun with --yes." >&2
    exit 2
  fi
  read -r -p "Type INSTALL to build the CLI and install the Codex skill: " answer
  [[ "$answer" == "INSTALL" ]] || { echo "Installation cancelled." >&2; exit 2; }
fi

cargo install --path "$repo_root/rust/mko-cli" --locked --force
[[ -x "$mko_binary" ]] || { echo "cargo reported success but mko was not found at $mko_binary." >&2; exit 1; }

if ((!skip_skill)); then
  skill_parent="$(dirname "$skill_target")"
  mkdir -p "$skill_parent"
  nonce="$$-$(date -u +%Y%m%dT%H%M%SZ)"
  stage="$skill_target.stage-$nonce"
  backup="$skill_target.backup-$nonce"
  mkdir "$stage"
  cp -R "$skill_source/." "$stage/"
  [[ -f "$stage/SKILL.md" ]] || { rm -rf "$stage"; echo "Staged skill is missing SKILL.md." >&2; exit 1; }
  if [[ -e "$skill_target" ]]; then
    mv "$skill_target" "$backup"
    echo "  previous skill backup: $backup"
  fi
  if ! mv "$stage" "$skill_target"; then
    [[ -e "$backup" && ! -e "$skill_target" ]] && mv "$backup" "$skill_target"
    exit 1
  fi
fi

version="$($mko_binary --version)"
echo
echo "Installed: $version"
if [[ ":${PATH}:" != *":$(dirname "$mko_binary"):"* ]]; then
  echo "Add $(dirname "$mko_binary") to PATH before opening a new Codex session."
fi
echo "Restart Codex so it reloads the skill. Then say: My Knowledge OS 시작해줘"
echo "The installer intentionally did not run mko setup or mutate a knowledge repository."
