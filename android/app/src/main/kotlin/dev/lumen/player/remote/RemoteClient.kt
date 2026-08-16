package dev.lumen.player.remote

import java.io.BufferedReader
import java.io.InputStreamReader
import java.io.OutputStreamWriter
import java.net.InetSocketAddress
import java.net.Socket
import java.net.SocketTimeoutException
import java.security.SecureRandom
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicLong
import javax.net.ssl.SSLContext
import javax.net.ssl.SSLSocket
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeout

/** Where the connection currently stands. */
sealed interface ConnectionState {
    data object Disconnected : ConnectionState
    data object Connecting : ConnectionState
    /** Connected, socket open, but neither a token nor a code has been accepted yet. */
    data object AwaitingPairing : ConnectionState
    data object Connected : ConnectionState
    data class Failed(val reason: String) : ConnectionState
}

/**
 * A live connection to one `lumen serve` instance.
 *
 * Owns its own [CoroutineScope] rather than borrowing the caller's, because the reader loop has to
 * keep running for exactly as long as the socket is open — tying it to a ViewModel's `viewModelScope`
 * would work today, but would make this class impossible to reuse or test without dragging a
 * ViewModel lifecycle along with it. [close] cancels the scope, which is what actually stops the
 * loop; closing the socket alone would leave a coroutine blocked in a read that will never return.
 *
 * Requests and their replies are matched by id, the same discipline the wire protocol was built
 * around: [send] registers a [CompletableDeferred] under the id it just wrote, and the reader loop
 * resolves it when a `reply` or `paired` message naming that id comes back. A `state` push carries no
 * id and is never treated as a reply to anything — it goes straight to [playback] instead.
 *
 * The socket is TLS, always -- `lumen serve` only accepts TLS connections (see the Rust side's
 * `remote/tls.rs`). [connect] takes a `pinnedFingerprint`: `null` for a first pairing, where any
 * certificate is accepted and recorded for the caller to persist once pairing actually succeeds; a
 * saved fingerprint for every reconnect after that, which [FingerprintTrustManager] enforces exactly.
 * See [observedFingerprint].
 */
class RemoteClient {

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private var socket: Socket? = null
    private var writer: OutputStreamWriter? = null
    private var readerJob: Job? = null
    private var trustManager: FingerprintTrustManager? = null
    private val nextId = AtomicLong(1)
    private val pending = ConcurrentHashMap<String, CompletableDeferred<RemoteProtocol.ServerMessage>>()

    private val _connection = MutableStateFlow<ConnectionState>(ConnectionState.Disconnected)
    val connection: StateFlow<ConnectionState> = _connection.asStateFlow()

    private val _playback = MutableStateFlow(RemoteProtocol.PlaybackState(null, 0))
    val playback: StateFlow<RemoteProtocol.PlaybackState> = _playback.asStateFlow()

    /** The fingerprint of the certificate the current (or most recent) connection actually presented
     * -- set the moment the TLS handshake completes, regardless of whether it was pinned or accepted
     * on first sight. `null` before any successful handshake. The caller persists this alongside the
     * token once pairing succeeds; see [RemoteViewModel.submitPairingCode]. */
    val observedFingerprint: String? get() = trustManager?.observedFingerprint

    /** Open a TLS connection and start the reader loop. Does not pair or authenticate — that is a
     * separate step, because the caller may have a saved token to try before falling back to asking
     * for a fresh pairing code.
     *
     * [pinnedFingerprint] is `null` only for a first pairing to a server this app has never connected
     * to before; every other caller should pass the fingerprint saved from that first pairing, so a
     * later connection is held to presenting the same certificate rather than trusting whatever shows
     * up that time. A mismatch fails the handshake itself -- surfaced through the normal
     * `ConnectionState.Failed` path below, not a special case.
     */
    suspend fun connect(host: String, port: Int, pinnedFingerprint: String?) {
        close()
        _connection.value = ConnectionState.Connecting
        try {
            val s = withContext(Dispatchers.IO) {
                val raw = Socket().apply { connect(InetSocketAddress(host, port), CONNECT_TIMEOUT_MS) }
                val tm = FingerprintTrustManager(pinnedFingerprint)
                val sslContext = SSLContext.getInstance("TLS")
                sslContext.init(null, arrayOf(tm), SecureRandom())
                val tls = sslContext.socketFactory.createSocket(raw, host, port, true) as SSLSocket
                tls.startHandshake()
                trustManager = tm
                tls
            }
            socket = s
            writer = OutputStreamWriter(s.getOutputStream(), Charsets.UTF_8)
            _connection.value = ConnectionState.AwaitingPairing
            readerJob = scope.launch { readLoop(s) }
        } catch (e: java.io.IOException) {
            // Covers both a plain connection failure and a rejected/mismatched TLS handshake --
            // startHandshake() throws SSLHandshakeException, an IOException subclass, wrapping
            // whatever FingerprintTrustManager threw, so its message (which names the mismatch)
            // reaches the UI through the same path as any other connection error.
            _connection.value = ConnectionState.Failed(e.message ?: "could not connect")
        }
    }

    private suspend fun readLoop(s: Socket) {
        val reader = BufferedReader(InputStreamReader(s.getInputStream(), Charsets.UTF_8))
        try {
            while (true) {
                val line = reader.readLine() ?: break // null means the peer closed the connection.
                if (line.isBlank()) continue
                when (val msg = RemoteProtocol.parseServerMessage(line)) {
                    is RemoteProtocol.ServerMessage.State -> _playback.value = msg.playback
                    is RemoteProtocol.ServerMessage.Paired -> pending.remove(msg.id)?.complete(msg)
                    is RemoteProtocol.ServerMessage.ReplyOk -> pending.remove(msg.id)?.complete(msg)
                    is RemoteProtocol.ServerMessage.ReplyError -> pending.remove(msg.id)?.complete(msg)
                    RemoteProtocol.ServerMessage.Unknown -> {} // Forward-compatible: just ignored.
                }
            }
        } catch (e: java.io.IOException) {
            // A read failing mid-loop is the same "connection is gone" event as a clean close from
            // the peer's side; both are handled identically below.
        }
        _connection.value = ConnectionState.Disconnected
        // Anyone still waiting on a reply that will now never arrive must be told so, rather than
        // hang forever — a caller awaiting `play()` when the socket drops deserves an answer.
        pending.values.forEach {
            it.complete(RemoteProtocol.ServerMessage.ReplyError("", "connection closed"))
        }
        pending.clear()
    }

    /** Send one request and wait for its reply, matched by id. */
    private suspend fun send(msg: RemoteProtocol.ClientMessage): RemoteProtocol.ServerMessage {
        val w = writer ?: return RemoteProtocol.ServerMessage.ReplyError("", "not connected")
        val id = nextId.getAndIncrement().toString()
        val deferred = CompletableDeferred<RemoteProtocol.ServerMessage>()
        pending[id] = deferred
        return try {
            withContext(Dispatchers.IO) {
                w.write(msg.toLine(id))
                w.write("\n")
                w.flush()
            }
            withTimeout(REPLY_TIMEOUT_MS) { deferred.await() }
        } catch (e: SocketTimeoutException) {
            pending.remove(id)
            RemoteProtocol.ServerMessage.ReplyError(id, "timed out: ${e.message}")
        } catch (e: kotlinx.coroutines.TimeoutCancellationException) {
            pending.remove(id)
            RemoteProtocol.ServerMessage.ReplyError(id, "timed out waiting for a reply")
        } catch (e: java.io.IOException) {
            pending.remove(id)
            RemoteProtocol.ServerMessage.ReplyError(id, e.message ?: "write failed")
        }
    }

    /** The pairing code shown on the server's terminal. On success, [connection] moves to
     * [ConnectionState.Connected] and the returned token should be saved for next time. */
    suspend fun pair(code: String): Result<String> =
        when (val reply = send(RemoteProtocol.ClientMessage.Pair(code))) {
            is RemoteProtocol.ServerMessage.Paired -> {
                _connection.value = ConnectionState.Connected
                Result.success(reply.token)
            }
            is RemoteProtocol.ServerMessage.ReplyError -> Result.failure(RemoteException(reply.message))
            else -> Result.failure(RemoteException("unexpected reply to pairing"))
        }

    /** A token saved from a previous [pair] call. */
    suspend fun authenticate(token: String): Result<Unit> =
        when (val reply = send(RemoteProtocol.ClientMessage.Auth(token))) {
            is RemoteProtocol.ServerMessage.ReplyOk -> {
                _connection.value = ConnectionState.Connected
                Result.success(Unit)
            }
            is RemoteProtocol.ServerMessage.ReplyError -> Result.failure(RemoteException(reply.message))
            else -> Result.failure(RemoteException("unexpected reply to authentication"))
        }

    suspend fun library(): Result<List<RemoteProtocol.LibraryEntry>> =
        when (val reply = send(RemoteProtocol.ClientMessage.Library)) {
            is RemoteProtocol.ServerMessage.ReplyOk -> Result.success(reply.library.orEmpty())
            is RemoteProtocol.ServerMessage.ReplyError -> Result.failure(RemoteException(reply.message))
            else -> Result.failure(RemoteException("unexpected reply to a library request"))
        }

    suspend fun play(path: String): Result<Unit> = simpleCommand(RemoteProtocol.ClientMessage.Play(path))
    suspend fun pause(): Result<Unit> = simpleCommand(RemoteProtocol.ClientMessage.Pause)
    suspend fun resume(): Result<Unit> = simpleCommand(RemoteProtocol.ClientMessage.Resume)
    suspend fun toggle(): Result<Unit> = simpleCommand(RemoteProtocol.ClientMessage.Toggle)
    suspend fun seek(positionMs: Long): Result<Unit> =
        simpleCommand(RemoteProtocol.ClientMessage.Seek(positionMs))
    suspend fun setVolume(level: Int): Result<Unit> =
        simpleCommand(RemoteProtocol.ClientMessage.SetVolume(level.coerceIn(0, 100)))

    private suspend fun simpleCommand(msg: RemoteProtocol.ClientMessage): Result<Unit> =
        when (val reply = send(msg)) {
            is RemoteProtocol.ServerMessage.ReplyOk -> Result.success(Unit)
            is RemoteProtocol.ServerMessage.ReplyError -> Result.failure(RemoteException(reply.message))
            else -> Result.failure(RemoteException("unexpected reply"))
        }

    /** Tear down the socket and stop the reader loop. Safe to call more than once, and safe to call
     * before ever connecting. */
    fun close() {
        readerJob?.cancel()
        readerJob = null
        runCatching { socket?.close() }
        socket = null
        writer = null
        pending.values.forEach {
            it.complete(RemoteProtocol.ServerMessage.ReplyError("", "connection closed"))
        }
        pending.clear()
        _connection.value = ConnectionState.Disconnected
    }

    /** Releases this client permanently — unlike [close], the coroutine scope itself is torn down,
     * so nothing about this instance can be reused afterwards. Call from `onCleared()`, not `close()`
     * followed by a later `connect()`. */
    fun release() {
        close()
        scope.cancel()
    }

    companion object {
        const val CONNECT_TIMEOUT_MS = 8_000
        const val REPLY_TIMEOUT_MS = 8_000L
    }
}

class RemoteException(message: String) : Exception(message)
