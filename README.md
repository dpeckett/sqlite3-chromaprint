# SQLite3 Chromaprint

Experimental SQLite3 extension for audio fingerprinting.

Useful for identifying similar audio files (e.g. duplicate songs).

## Usage

```sql
-- Load the extension
.load ./libsqlite3_chromaprint

-- Compute the fingerprint of two audio files
-- and compare them. The result is a similarity
-- score between 0 (highest similarity) and 32 (lowest similarity).
SELECT compare_fingerprints(
  fingerprint('track1.mp3'),
  fingerprint('track2.mp3')
);
```