// Finder's Quick Look preview for the data files DuckLocal opens: the
// columns and first rows, rendered by the Rust engine (quicklook/src) as
// HTML. A data-based preview: Quick Look asks for the page and draws it,
// so this process does nothing but read the file and answer.

import Foundation
import QuickLookUI
import UniformTypeIdentifiers

final class PreviewProvider: QLPreviewProvider, QLPreviewingController {
    func providePreview(for request: QLFilePreviewRequest) async throws -> QLPreviewReply {
        let html = request.fileURL.path.withCString { path -> String in
            guard let page = ducklocal_preview_html(path) else { return "" }
            defer { ducklocal_preview_free(page) }
            return String(cString: page)
        }
        let data = Data(html.utf8)
        return QLPreviewReply(
            dataOfContentType: .html,
            contentSize: CGSize(width: 960, height: 640)
        ) { reply in
            reply.stringEncoding = .utf8
            return data
        }
    }
}
