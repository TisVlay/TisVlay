# Shot 2.4's terminal: the CLI reads and sets what the GUI does.
# Recorded by the driver against its stack; $DESK is that TV's id.
Set Shell bash
Set Width 3840
Set Height 2160
Set FontSize 64
Set Padding 120
Set Framerate 30
Set TypingSpeed 60ms
Set Theme { "name": "Breeze Dark", "background": "#141618", "foreground": "#fcfcfc", "cursor": "#3daee9", "selection": "#3daee9", "black": "#232627", "red": "#ed1515", "green": "#11d116", "yellow": "#f67400", "blue": "#1d99f3", "magenta": "#9b59b6", "cyan": "#1abc9c", "white": "#fcfcfc", "brightBlack": "#7f8c8d", "brightRed": "#c0392b", "brightGreen": "#1cdc9a", "brightYellow": "#fdbc4b", "brightBlue": "#3daee9", "brightMagenta": "#8e44ad", "brightCyan": "#16a085", "brightWhite": "#ffffff" }

Type "tisvlay list"
Enter
Sleep 2s
Type "tisvlay volume $DESK"
Enter
Sleep 1500ms
Type "tisvlay volume $DESK 45"
Enter
Sleep 1s
Type "tisvlay volume $DESK"
Enter
Sleep 2s
