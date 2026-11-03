# Command boundaries are session-local; never export the installation guard.
[[ $- == *i* ]] || return 0
[[ ${__oxide_command_hooks:-} == 1 ]] && return 0
case "${PS1-}${PS0-}" in
    *']133;'*|*']633;'*) return 0 ;;
esac

__oxide_command_end() {
    local result=$?
    __oxide_at_prompt=0
    if [[ ${__oxide_prompt_seen:-} == 1 ]]; then
        builtin printf '\033]133;D;%s\007' "$result"
    fi
    __oxide_prompt_seen=1
    return "$result"
}

__oxide_command_prompt() {
    local result=$?
    # Prompt themes may replace PS1 on every command. Wrap their latest value.
    if [[ ${PS1-} != "${__oxide_wrapped_prompt-}" ]]; then
        case "${PS1-}" in
            *']133;'*|*']633;'*) __oxide_at_prompt=0; return "$result" ;;
        esac
        __oxide_wrapped_prompt='\[\e]133;A\a\]'"${PS1-}"'\[\e]133;B\a\]'
        PS1=$__oxide_wrapped_prompt
    fi
    __oxide_at_prompt=1
    return "$result"
}

__oxide_command_start() {
    [[ ${BASH_SUBSHELL:-0} == 0 && -z ${COMP_LINE+x} ]] || return 0
    [[ ${__oxide_at_prompt:-0} == 1 ]] || return 0
    case "$BASH_COMMAND" in
        __oxide_command_end*|__oxide_command_prompt*) return 0 ;;
    esac
    __oxide_at_prompt=0
    builtin printf '\033]133;C\007'
}

if (( BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4) )); then
    # PS0 runs once for an accepted command, including multiline input, without
    # taking ownership of DEBUG traps installed by the user's configuration.
    PS0=$'\e]133;C\a'"${PS0-}"
elif [[ -n ${bash_preexec_imported:-} ]]; then
    preexec_functions+=(__oxide_command_start)
elif [[ -z $(trap -p DEBUG) ]]; then
    trap '__oxide_command_start' DEBUG
else
    # Older Bash has no PS0. Do not replace an unrelated DEBUG trap.
    return 0
fi

if [[ $(declare -p PROMPT_COMMAND 2>/dev/null) == 'declare -a '* ]]; then
    PROMPT_COMMAND=(__oxide_command_end "${PROMPT_COMMAND[@]}" __oxide_command_prompt)
else
    PROMPT_COMMAND=$'__oxide_command_end\n'"${PROMPT_COMMAND-}"$'\n__oxide_command_prompt'
fi
__oxide_command_hooks=1
