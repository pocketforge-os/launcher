# Known baseline failures

The pre-Taffy raster guard fails for Home and Quick overlay at the 960×540 surface at
100%, 150%, and 200% text scale. The exact failing node diagnostics are preserved in
the six corresponding JSON records. These are known pre-existing results and were not
normalized or repaired by this spike.

Some remaining records say `NOT_RUN` when a capture was interrupted before the
expensive paired-raster guard completed. That is deliberately an explicit incomplete
result rather than a fabricated pass; rerunning the exact README command replaces it.
