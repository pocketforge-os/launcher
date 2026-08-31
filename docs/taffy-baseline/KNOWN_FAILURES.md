# Known baseline failures

The pre-Taffy raster guard fails for Home and Quick overlay at the 960×540 surface at
100%, 150%, and 200% text scale. The exact failing node diagnostics are preserved in
the six corresponding JSON records. It also fails for Library at 1920×1080 at all
three text scales because the final row's labels are below the surface. The exact
diagnostics are preserved in those three records. These are known pre-existing results
and were not normalized or repaired by this spike.
