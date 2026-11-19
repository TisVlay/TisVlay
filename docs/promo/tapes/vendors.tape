# Shot 2.5's terminal: the vendor backends the daemon carries, and what
# one of them can do.
Set Shell bash
Set Width 2880
Set Height 1500
Set FontSize 52
Set Padding 72
Set Framerate 30
Set TypingSpeed 60ms
Set Theme { "name": "Breeze Dark", "background": "#141618", "foreground": "#fcfcfc", "cursor": "#3daee9", "selection": "#3daee9", "black": "#232627", "red": "#ed1515", "green": "#11d116", "yellow": "#f67400", "blue": "#1d99f3", "magenta": "#9b59b6", "cyan": "#1abc9c", "white": "#fcfcfc", "brightBlack": "#7f8c8d", "brightRed": "#c0392b", "brightGreen": "#1cdc9a", "brightYellow": "#fdbc4b", "brightBlue": "#3daee9", "brightMagenta": "#8e44ad", "brightCyan": "#16a085", "brightWhite": "#ffffff" }

# A plain prompt in the accent color, set out of sight.
Hide
Type "PS1='\[\e[38;2;61;174;233m\]$\[\e[0m\] ' && clear"
Enter
Show

Type "tisvlay vendor"
Enter
Sleep 2500ms
Type "tisvlay vendor cec"
Enter
Sleep 4s
