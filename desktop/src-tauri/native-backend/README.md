# Native backend payload

Release packaging places the current platform's unpacked, checksummed package
here before the Tauri bundle is built. Keeping the signed executables unpacked
lets Apple notarisation inspect them; the enclosing DMG/AppImage still provides
compression, and Windows packages the same tree inside NSIS. Diagnostic builds
may stage a `.tar.gz` instead, and development
can set `MAGICIAN_PACKAGE` to the package produced by `make package-release`.

The Desktop setup flow keeps native installation unavailable when no compatible
payload is present.
