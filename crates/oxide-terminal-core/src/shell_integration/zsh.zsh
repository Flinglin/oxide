[[ -o interactive ]] || return 0
(( ${+__oxide_command_hooks} )) && return 0
case "$PS1" in
    *']133;'*|*']633;'*) return 0 ;;
esac

__oxide_command_end() {
    local result=$?
    if (( ${__oxide_command_running:-0} )); then
        builtin printf '\033]133;D;%s\007' "$result"
    fi
    __oxide_command_running=0
    return "$result"
}

__oxide_command_prompt() {
    local result=$?
    if [[ "$PS1" != "${__oxide_wrapped_prompt-}" ]]; then
        case "$PS1" in
            *']133;'*|*']633;'*) __oxide_command_enabled=0; return "$result" ;;
        esac
        __oxide_wrapped_prompt=$'%{\e]133;A\a%}'"$PS1"$'%{\e]133;B\a%}'
        PS1=$__oxide_wrapped_prompt
    fi
    __oxide_command_enabled=1
    return "$result"
}

__oxide_command_start() {
    (( ${__oxide_command_enabled:-0} )) || return 0
    __oxide_command_running=1
    builtin printf '\033]133;C\007'
}

# Capture the exit status before other precmd hooks and wrap the prompt after
# theme hooks. All existing hook functions keep their original relative order.
typeset -ga precmd_functions preexec_functions
precmd_functions=(__oxide_command_end "${precmd_functions[@]}" __oxide_command_prompt)
preexec_functions+=(__oxide_command_start)
typeset -g __oxide_command_hooks=1
