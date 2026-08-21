package io.xberg.crawlberg.android

import com.fasterxml.jackson.module.kotlin.jacksonObjectMapper
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class CrawlbergScaffoldTest {
    // Round-trips the generated `HreflangEntry` data class through the Jackson mapping the
    // JNI bridge marshals values with: it fails to compile if the generated constructor
    // loses a parameter or changes a type, and fails at runtime if the class stops being
    // serializable or stops rebuilding an equal value. It proves nothing about the native
    // library -- no tier here loads it, deliberately; see the note on the emitter. Seeded
    // once and never regenerated over, so replace it with a real suite. ~keep
    @Test
    fun hreflangEntryRoundTripsThroughItsGeneratedJsonMapping() {
        val original = HreflangEntry("alef-scaffold", "alef-scaffold")
        val mapper = jacksonObjectMapper()
        val json = mapper.writeValueAsString(original)
        assertTrue(
            "serialized payload lost the constructed value: $json",
            json.contains("alef-scaffold"),
        )
        assertEquals(original, mapper.readValue(json, HreflangEntry::class.java))
    }
}
