// PairingApi.kt — device pairing (Phase 77 S6). Reached via client.pairing.
package dev.winmux.sdk

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.encodeToString

class PairingApi(private val c: WinmuxClient) {
    @Serializable
    private data class RedeemBody(@SerialName("one_shot_token") val oneShotToken: String)

    /**
     * POST /api/pairing/redeem — exchange a one-shot pairing token (from the
     * desktop's QR) for a durable device credential. Public: the one-shot token
     * itself is the auth. The response also carries default_workspace_id so the
     * phone can connect without a workspace-list round-trip.
     */
    fun redeem(oneShotToken: String): PairingRedeemResponse =
        c.json.decodeFromString(c.postText("/api/pairing/redeem", c.json.encodeToString(RedeemBody(oneShotToken))))
}
