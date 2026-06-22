# Packaged by spice; do not edit.
: "${SPICE_SHELL_HOOK_PYTHON:?spice shell hook: missing SPICE_SHELL_HOOK_PYTHON}"

_spice_shell_hook_name=".zprofile"
_spice_shell_hook_dir="${ZDOTDIR:?spice shell hook: missing ZDOTDIR}"
_spice_shell_hook_self="${_spice_shell_hook_dir}/${_spice_shell_hook_name}"

if [ -n "${ZSH_EXECUTION_STRING-}" ]; then
  _spice_shell_bin="${SHELL:-${ZSH_NAME:-zsh}}"
  case "$_spice_shell_bin" in
    *zsh) ;;
    *) _spice_shell_bin="${ZSH_NAME:-zsh}" ;;
  esac
  if ! command -v "$_spice_shell_bin" >/dev/null 2>&1; then
    printf "%s\n" "spice shell hook: cannot resolve zsh for agent-run reexec" >&2
    exit 127
  fi
  if [[ -o login ]]; then
    unset ZDOTDIR
    unset BASH_ENV
    exec "$SPICE_SHELL_HOOK_PYTHON" -m spice agent run -- "$_spice_shell_bin" -lc "$ZSH_EXECUTION_STRING"
    printf "%s\n" "spice shell hook: failed to exec agent run" >&2
    exit 127
  fi
  unset ZDOTDIR
  unset BASH_ENV
  exec "$SPICE_SHELL_HOOK_PYTHON" -m spice agent run -- "$_spice_shell_bin" -c "$ZSH_EXECUTION_STRING"
  printf "%s\n" "spice shell hook: failed to exec agent run" >&2
  exit 127
fi
case $- in
  *i*) ;;
  *)
    printf "%s\n" "spice shell hook: cannot agent-run reexec noninteractive shell without an execution string" >&2
    exit 127
    ;;
esac

set -o pipefail

if [ -n "${SPICE_SHELL_HOOK_ORIGINAL_ZDOTDIR-}" ]; then
  export ZDOTDIR="$SPICE_SHELL_HOOK_ORIGINAL_ZDOTDIR"
else
  unset ZDOTDIR
fi
if [ -n "${SPICE_SHELL_HOOK_ORIGINAL_BASH_ENV-}" ]; then
  export BASH_ENV="$SPICE_SHELL_HOOK_ORIGINAL_BASH_ENV"
else
  unset BASH_ENV
fi
if [ -n "${SPICE_SHELL_HOOK_ORIGINAL_HISTFILE-}" ]; then
  export HISTFILE="$SPICE_SHELL_HOOK_ORIGINAL_HISTFILE"
fi

_spice_shell_real_zdotdir="${SPICE_SHELL_HOOK_ORIGINAL_ZDOTDIR:-${HOME:-}}"
_spice_shell_real_source="${_spice_shell_real_zdotdir}/${_spice_shell_hook_name}"
if [ -r "$_spice_shell_real_source" ] && [ "$_spice_shell_real_source" != "$_spice_shell_hook_self" ]; then
  . "$_spice_shell_real_source"
fi

_spice_shell_static_hook_dir="${_spice_shell_hook_dir%/shellhooks}/shellhooks2"
export ZDOTDIR="$_spice_shell_static_hook_dir"
export BASH_ENV="${_spice_shell_static_hook_dir}/bash_env"
eval "${SPICE_SHELL_HOOK_WRAPPERS-}"

unset _spice_shell_bin
unset _spice_shell_hook_dir
unset _spice_shell_hook_name
unset _spice_shell_hook_self
unset _spice_shell_real_source
unset _spice_shell_real_zdotdir
unset _spice_shell_static_hook_dir
