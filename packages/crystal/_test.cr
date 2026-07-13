s = %({"a":{"b":1}})
puts s
puts s.bytesize
handle = LibCberg.crawl_config_from_json("{}")
puts "Empty JSON config: #{handle.null? ? "NULL" : "OK"}"
LibCberg.crawl_config_free(handle) unless handle.null?
