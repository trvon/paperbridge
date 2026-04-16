# Shell Completions

Generate a completion script for your shell and install it:

```bash
# bash (Linux)
paperbridge completions bash | sudo tee /etc/bash_completion.d/paperbridge

# bash (macOS, Homebrew)
paperbridge completions bash > "$(brew --prefix)/etc/bash_completion.d/paperbridge"

# zsh — install to fpath, then enable compinit in ~/.zshrc
mkdir -p ~/.zfunc
paperbridge completions zsh > ~/.zfunc/_paperbridge
# add to ~/.zshrc (once):
#   fpath=(~/.zfunc $fpath)
#   autoload -U compinit && compinit

# zsh — or source directly per-shell (no install):
#   source <(paperbridge completions zsh)

# fish
paperbridge completions fish > ~/.config/fish/completions/paperbridge.fish

# PowerShell — add to $PROFILE
paperbridge completions powershell | Out-String | Invoke-Expression
```

Supported shells: `bash`, `zsh`, `fish`, `powershell`, `elvish`.
