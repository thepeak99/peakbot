# Release Notes (Working Draft)

This file is the working draft for the next release. When a version is tagged, this file is renamed to `<version>.md` and a new empty `current.md` is created.

## Changes

- **The web transcript no longer yanks you to the bottom while you're reading history.** It follows the newest message only while you're *pinned* to the tail (within 80px of the bottom); scroll up and new messages stay out of your way. When messages arrive while you're up in the history, a floating **"↓ N new messages"** button appears bottom-right of the transcript — clicking it smooth-scrolls to the newest message and re-pins (it reads just "↓ bottom" when nothing new landed). A matching **"↑ top"** button appears once you're more than 400px down and jumps to the top of the loaded transcript. Sending a message always re-pins, because you expect to see what you just sent. Only scrolling *up* unpins, so a smooth scroll toward the tail can't fight the follow it just started.
