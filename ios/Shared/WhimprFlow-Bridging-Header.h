// Exposes the whimpr-core C ABI to Swift. Both targets use this same header — the
// keyboard extension needs the core as much as the app does, because the text it
// inserts has already been through the gates.
#import "whimpr.h"
