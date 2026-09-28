# native-apfs.dmg

A 17,777-byte compressed (UDZO) APFS DMG created by the parent on real
macOS 15.7.7 with:

    hdiutil create -srcfolder <folder> -format UDZO native-probe.dmg

Contents are synthetic; no user app is included:

- `Probe.app/Contents/Info.plist` — plain file
- `Probe.app/Contents/Versions/A/probe` — shell script, prints `fixture-ok`
- `Probe.app/Contents/Versions`, `Probe.app/Contents` — plain directories
- `Probe.app/Contents/Current -> Versions/A` — relative in-bundle symlink
- `Applications -> /Applications` — the conventional DMG shortcut

Purpose: `7zz l -ba -slt` on APFS emits `Symbolic Link = ` with an EMPTY
value for ordinary dirs and files (the `Mode = drwx.../-rw...` record is
authoritative; real links carry `Mode = l...` plus a nonempty value).
This fixture regression-tests the misclassification that read every
directory as a dangling zero-length link. No vendor file is inside this
DMG.
