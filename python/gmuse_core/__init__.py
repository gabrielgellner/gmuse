"""gmuse's library, for the tools that prepare files for it — #197.

A mixed layout: the compiled extension sits beside this file and the stubs
travel with it, so the tools keep their type checking. This module exists to
re-export, because a package directory would otherwise shadow the extension it
is meant to carry.
"""

from .gmuse_core import Library, __version__

__all__ = ["Library", "__version__"]
