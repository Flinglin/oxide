[[ -o interactive ]] || return 0
typeset -g __oxide_prompt_start=$'%{\e]133;A;aid=oxide\a%}'
typeset -g __oxide_prompt_end=$'%{\e]133;B;aid=oxide\a%}'
typeset -ga precmd_functions preexec_functions
# Reloading configuration repairs our hooks without retaining old wrappers.
precmd_functions=(${precmd_functions:#__oxide_command_end})
precmd_functions=(${precmd_functions:#__oxide_command_prompt})
preexec_functions=(${preexec_functions:#__oxide_command_start})
PS1=${PS1//"$__oxide_prompt_start"/}
PS1=${PS1//"$__oxide_prompt_end"/}

# Powerlevel10k rebuilds PS1 after precmd and during asynchronous redraws.
# Use its own markers so prompt wrapping cannot be overwritten by the theme.
if (( ${+functions[p10k]} )); then
    typeset -g POWERLEVEL9K_TERM_SHELL_INTEGRATION=true
    return 0
fi

__oxide_command_end() {
    local result=$?
    # A theme may call precmd from the line editor just to redraw the prompt.
    builtin zle && return "$result"
    if (( ${__oxide_command_running:-0} )); then
        builtin printf '\033]133;D;%s;aid=oxide\007' "$result"
    fi
    __oxide_command_running=0
    return "$result"
}

__oxide_command_prompt() {
    local result=$?
    PS1=${PS1//"$__oxide_prompt_start"/}
    PS1=${PS1//"$__oxide_prompt_end"/}
    case "$PS1" in
        *']133;'*|*']633;'*) __oxide_command_enabled=0; return "$result" ;;
    esac
    PS1="$__oxide_prompt_start$PS1$__oxide_prompt_end"
    __oxide_command_enabled=1
    return "$result"
}

__oxide_command_start() {
    (( ${__oxide_command_enabled:-0} )) || return 0
    __oxide_command_running=1
    builtin printf '\033]133;C;aid=oxide\007'
}

# Capture the exit status before other precmd hooks and wrap the prompt after
# theme hooks. All existing hook functions keep their original relative order.
precmd_functions=(__oxide_command_end "${precmd_functions[@]}" __oxide_command_prompt)
preexec_functions+=(__oxide_command_start)
