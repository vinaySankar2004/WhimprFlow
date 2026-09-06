`words-20k.txt` — the 20,000 most common English words, one per line, most frequent
first. From <https://github.com/first20hours/google-10000-english> (file `20k.txt`),
derived from Google's Trillion Word Corpus n-gram data; the repository states no
license restrictions. Fetched 2026-09-05.

Used by `SwipeDecoder` to turn a finger path into a word: the order is the frequency
prior, which is what breaks ties between words that share a shape on the keyboard.
Autocorrect does not use it — that is Apple's `UITextChecker`.
