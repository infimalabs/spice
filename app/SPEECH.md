# Browser speech

Manual **Play** remains available when the team speech mode is **Quiet**; Quiet suppresses automatic speech, not an explicit request.

A browser request that needs a published voice may initially find no system
voices and later receive them through `voiceschanged`. Each open admission
within the playback generation accepts one such notification. The callback
consumes that admission synchronously before deferring its continuation, so a
burst of duplicate notifications cannot restart one request. If the
continuation still finds no voices, it opens one fresh admission for a later
publication. If no publication yields voices within five seconds of the first
wait, the request fails and the queue advances. Stop, replacement, completion,
and page teardown invalidate every admission with the playback generation.

Browser utterance chunks retain the same ownership rule: each current chunk
can start its successor once. Intermediate ends never settle the queue entry;
the final end or an utterance error does. Stop and replacement invalidate
every retained callback before releasing the synthesis chain.
