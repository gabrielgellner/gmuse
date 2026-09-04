"""Out-of-tree helpers for the gmuse library.

**None of this is gmuse.** §1 puts tag editing out of scope, and the pattern is
the one rsgain and bliss already set: an external tool writes, gmuse reads.
These passes measure, enrich and identify; what they produce is a file of
proposed facts that `gmuse library apply` takes or refuses.

A package rather than a directory of scripts, because a script's own directory
lands on `sys.path` and three imports here once resolved only by accident.
"""
