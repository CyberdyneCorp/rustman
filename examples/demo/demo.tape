# The README demo (docs/images/demo.gif): rustman's TUI and CLI against the
# todo backend in backend.py. Record it with render.sh, which builds rustman,
# copies the workspace to $DEMO_DIR, stores the demo token in the file secret
# store and starts the backend; `vhs` alone would find no backend.

Output docs/images/demo.gif

Require rustman

Set Shell "bash"
# Tokyo Night, the palette of rustman's default dark theme.
Set Theme { "name": "rustman-dark", "black": "#15161e", "red": "#f7768e", "green": "#9ece6a", "yellow": "#e0af68", "blue": "#7aa2f7", "magenta": "#bb9af7", "cyan": "#7dcfff", "white": "#a9b1d6", "brightBlack": "#414868", "brightRed": "#f7768e", "brightGreen": "#9ece6a", "brightYellow": "#e0af68", "brightBlue": "#7aa2f7", "brightMagenta": "#bb9af7", "brightCyan": "#7dcfff", "brightWhite": "#c0caf5", "background": "#1a1b26", "foreground": "#c0caf5", "selection": "#283457", "cursor": "#c0caf5" }
Set WindowBar Colorful
Set FontSize 16
Set Width 1280
Set Height 720
Set Padding 16
Set Framerate 15
Set TypingSpeed 60ms
Set CursorBlink false

# A neutral prompt and the demo workspace: no paths or user names on screen.
Hide
Type `export PS1='$ ' COLORTERM=truecolor RUSTMAN_SECRET_STORE=file && cd "$DEMO_DIR" && clear`
Enter
Sleep 300ms
Show

# a. Open the TUI. The sidebar lists the workspace's requests.
Type "rustman"
Sleep 500ms
Enter
Sleep 1.5s

# The environment switcher, from the command palette (Ctrl+P, "swenv"): the
# local environment's variables, with the bearer token masked. Its own key,
# Alt+E, is not used here: headless Chrome on macOS turns Option+E into a dead
# key. Esc closes it.
Ctrl+P
Sleep 600ms
Type "swenv"
Sleep 600ms
Enter
Sleep 2.5s
Escape
Sleep 500ms

# b. Open "list-todos" (Enter), send it (Ctrl+R), focus the response and fold
#    one node of the JSON tree.
Down 2
Sleep 500ms
Enter
Sleep 800ms
Ctrl+R
Sleep 1.5s
Tab 3
Sleep 700ms
Down
Sleep 400ms
Enter
Sleep 1s

# c. Back to the sidebar (Tab), open "create-todo", show its JSON body (editor
#    tab 3), send it, then the response (Body, then Headers with the captured id).
Tab
Sleep 400ms
Down
Sleep 400ms
Enter
Sleep 600ms
Tab
Type "3"
Sleep 1.2s
Ctrl+R
Sleep 1.5s
Tab 2
Sleep 1.5s
Type "]"
Sleep 2s

# d. The Timing and Assertions tabs.
Type "]"
Sleep 2s
Type "]"
Sleep 2.5s

# e. Quit.
Ctrl+Q
Sleep 800ms

# f. The same requests as a test run from the CLI.
Type "rustman run --env local"
Sleep 500ms
Enter
Sleep 4s
