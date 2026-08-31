import Foundation
import Testing

@testable import BeamPlayback

/// Reading the length of a stream out of a range response.
@Suite("HTTP byte source")
struct ByteSourceTests {
    @Test("the total comes out of Content-Range")
    func parsesTotal() {
        #expect(HTTPByteSource.total(fromContentRange: "bytes 0-0/12345") == 12345)
        #expect(HTTPByteSource.total(fromContentRange: "bytes 100-199/4294967296") == 4_294_967_296)
    }

    @Test("an unknown total is refused rather than guessed")
    func unknownTotalIsRefused() {
        // "*" is legal and means the server will not say. A demuxer cannot
        // seek without a length, and inventing one would turn every seek past
        // the guess into a corrupt-file report.
        #expect(HTTPByteSource.total(fromContentRange: "bytes 0-0/*") == nil)
        #expect(HTTPByteSource.total(fromContentRange: "bytes 0-0") == nil)
        #expect(HTTPByteSource.total(fromContentRange: "") == nil)
    }
}
