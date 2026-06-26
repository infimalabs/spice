"""`python -m spice` — the installation-independent entrypoint.

The agent supervisor respawns through this module so a detached process finds
the same installed interpreter and package runtime.
"""

from spice.cli.entry import main

raise SystemExit(main())
