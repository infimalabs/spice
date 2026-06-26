"""`python -m spice` — module entrypoint for the loaded spice package.

The uv tool console script points at ``spice.cli.entry:main``; this module keeps
``python -m spice`` equivalent inside whichever installed environment launched
it, including detached supervisor respawns.
"""

from spice.cli.entry import main

raise SystemExit(main())
