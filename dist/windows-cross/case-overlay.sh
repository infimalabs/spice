#!/bin/sh
# Writes to stdout a case-insensitive Clang/LLD virtual filesystem overlay
# naming every file below the MSVC sysroot directory named by $1.
#
# Windows sources and libraries name headers and import libraries in whatever
# case suits them, for example `windows.h` for `Windows.h` or `kernel32.lib`
# for `kernel32.Lib`. The sysroot is copied from the admitting host, which can
# carry no case-variant links, so both tools resolve names through this overlay.
set -eu

find "$1" -type f | LC_ALL=C sort | awk '
function quoted(text) {
    gsub(/\\/, "\\\\", text)
    gsub(/"/, "\\\"", text)
    return "\"" text "\""
}

{
    match($0, /\/[^\/]*$/)
    directory = substr($0, 1, RSTART - 1)
    name = substr($0, RSTART + 1)
    if (!(directory in entries)) {
        order[++directories] = directory
        entries[directory] = ""
    } else {
        entries[directory] = entries[directory] ","
    }
    entries[directory] = entries[directory] "{\"name\":" quoted(name) \
        ",\"type\":\"file\",\"external-contents\":" quoted($0) "}"
}

END {
    printf "{\"version\":0,\"case-sensitive\":\"false\",\"roots\":["
    for (position = 1; position <= directories; position++) {
        if (position > 1) printf ","
        printf "{\"name\":%s,\"type\":\"directory\",\"contents\":[%s]}", \
            quoted(order[position]), entries[order[position]]
    }
    printf "]}\n"
}'
