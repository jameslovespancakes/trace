module Match where

-- | Whether a pattern matches a string exactly.
matches :: String -> String -> Bool
matches pat s = pat == s

-- | Count matching strings.
countMatches :: String -> [String] -> Int
countMatches pat = length . filter (matches pat)
