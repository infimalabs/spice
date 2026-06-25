# Packaged by spice; do not edit.
: "${SPICE_SHELL_HOOK_PYTHON:?spice shell hook: missing SPICE_SHELL_HOOK_PYTHON}"

_spice_shell_hook_name=".zshrc"
_spice_shell_hook_dir="${ZDOTDIR:?spice shell hook: missing ZDOTDIR}"
_spice_shell_hook_self="${_spice_shell_hook_dir}/${_spice_shell_hook_name}"

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

_spice_shell_static_hook_dir="${_spice_shell_hook_dir%/shellhooks}/staticshellhooks"
export ZDOTDIR="$_spice_shell_static_hook_dir"
export BASH_ENV="${_spice_shell_static_hook_dir}/bash_env"
eval "${SPICE_SHELL_HOOK_WRAPPERS-}"

unset _spice_shell_hook_dir
unset _spice_shell_hook_name
unset _spice_shell_hook_self
unset _spice_shell_real_source
unset _spice_shell_real_zdotdir
unset _spice_shell_static_hook_dir
