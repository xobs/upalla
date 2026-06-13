#!/bin/bash
pactl list short modules 2>/dev/null | while read idx rest; do
    mod_info=$(pactl list modules 2>/dev/null | grep -A10 "Module #$idx" | grep -i "upalla")
    if [ -n "$mod_info" ]; then
        pactl unload-module "$idx" 2>/dev/null && echo "Unloaded module $idx"
    fi
done
