// The C surface of the Rust preview engine (quicklook/src/lib.rs).
#include <stddef.h>

// The preview of the file at `path` as an HTML page; free it with
// ducklocal_preview_free. A file that cannot be read gets a page saying why.
char *ducklocal_preview_html(const char *path);
void ducklocal_preview_free(char *html);
