# Command hooks and their state belong to this interactive shell.
[[ $- == *i* ]] || return 0
# Tag our optional OSC parameters so native integration can own the same cycle.
__oxide_prompt_start='\[\e]133;A;aid=oxide\a\]'
__oxide_prompt_end='\[\e]133;B;aid=oxide\a\]'

__oxide_command_end() {
    local result=$?
    __oxide_at_prompt=0
    if [[ ${__oxide_prompt_seen:-} == 1 ]]; then
        builtin printf '\033]133;D;%s;aid=oxide\007' "$result"
    fi
    __oxide_prompt_seen=0
    return "$result"
}

__oxide_command_prompt() {
    local result=$?
    # Remove only our own wrappers before inspecting the theme's latest prompt.
    PS1=${PS1//"$__oxide_prompt_start"/}
    PS1=${PS1//"$__oxide_prompt_end"/}
    __oxide_command_execution=
    case "${PS1-}${PS0-}" in
        *']133;'*|*']633;'*) __oxide_at_prompt=0; return "$result" ;;
    esac
    PS1="$__oxide_prompt_start$PS1$__oxide_prompt_end"
    __oxide_command_execution=$'\e]133;C;aid=oxide\a'
    __oxide_prompt_seen=1
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
    builtin printf '\033]133;C;aid=oxide\007'
}

if (( BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4) )); then
    # PS0 runs once for an accepted command, including multiline input, without
    # taking ownership of DEBUG traps installed by the user's configuration.
    # A native prompt loaded later disables this expansion on the next prompt.
    PS0=${PS0#'${__oxide_command_execution}'}
    PS0='${__oxide_command_execution}'"${PS0-}"
elif [[ -n ${bash_preexec_imported:-} ]]; then
    __oxide_preexec=()
    for __oxide_hook in "${preexec_functions[@]}"; do
        [[ $__oxide_hook == __oxide_command_start ]] || __oxide_preexec+=("$__oxide_hook")
    done
    preexec_functions=("${__oxide_preexec[@]}" __oxide_command_start)
    unset __oxide_preexec __oxide_hook
elif [[ -z $(trap -p DEBUG) || $(trap -p DEBUG) == *'__oxide_command_start'* ]]; then
    trap '__oxide_command_start' DEBUG
else
    # Older Bash has no PS0. Do not replace an unrelated DEBUG trap.
    return 0
fi

if [[ $(declare -p PROMPT_COMMAND 2>/dev/null) == 'declare -a '* ]]; then
    __oxide_prompt_commands=()
    for __oxide_hook in "${PROMPT_COMMAND[@]}"; do
        case "$__oxide_hook" in
            __oxide_command_end|__oxide_command_prompt) ;;
            *) __oxide_prompt_commands+=("$__oxide_hook") ;;
        esac
    done
    PROMPT_COMMAND=(__oxide_command_end "${__oxide_prompt_commands[@]}" __oxide_command_prompt)
    unset __oxide_prompt_commands __oxide_hook
else
    PROMPT_COMMAND=${PROMPT_COMMAND#__oxide_command_end$'\n'}
    PROMPT_COMMAND=${PROMPT_COMMAND%$'\n'__oxide_command_prompt}
    PROMPT_COMMAND=$'__oxide_command_end\n'"${PROMPT_COMMAND-}"$'\n__oxide_command_prompt'
fi
