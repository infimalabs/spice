"""`python -m spice` — the installation-independent entrypoint.

The agent supervisor respawns through this module so a detached process finds
the same interpreter. Worktree source checkouts are put first on PYTHONPATH;
ordinary target repos use the installed package.
"""

from spice.cli.entry import main

raise SystemExit(main())
