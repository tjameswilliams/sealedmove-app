// C ABI shim over an in-process Stockfish 11.
//
// Stockfish is a program, not a library: it talks UCI over stdin/stdout and
// expects to own the process. This shim turns it into an embeddable engine
// by redirecting the global std::cin/std::cout stream buffers to
// mutex+condvar line queues and running Stockfish's normal main() init
// sequence + UCI::loop on a dedicated std::thread. The host process pushes
// command lines with sf_send() and pops output lines with sf_recv().
//
// Because the redirection is of the GLOBAL std::cin/std::cout rdbufs, there
// can be exactly ONE embedded instance per process — sf_start() enforces
// that with a singleton guard.
//
// Licensing: this file is part of the stockfish-embedded crate and links
// against Stockfish (GPLv3); the combined work is GPLv3. See ../README.md.

#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstdint>
#include <cstring>
#include <deque>
#include <iostream>
#include <mutex>
#include <streambuf>
#include <string>
#include <thread>
#include <unordered_map>

// Stockfish internal headers (vendor/stockfish is on the include path).
#include "bitboard.h"
#include "endgame.h"
#include "position.h"
#include "search.h"
#include "syzygy/tbprobe.h"
#include "thread.h"
#include "tt.h"
#include "uci.h"

namespace PSQT {
void init();
}

namespace {

// A blocking, closable queue of complete lines (no trailing '\n').
class LineQueue {
public:
    void push(std::string line) {
        {
            std::lock_guard<std::mutex> lock(mutex_);
            if (closed_)
                return;
            queue_.push_back(std::move(line));
        }
        cv_.notify_one();
    }

    // Block until a line is available or the queue is closed.
    // Returns false only when closed and drained.
    bool pop(std::string& out) {
        std::unique_lock<std::mutex> lock(mutex_);
        cv_.wait(lock, [&] { return !queue_.empty() || closed_; });
        if (queue_.empty())
            return false;
        out = std::move(queue_.front());
        queue_.pop_front();
        return true;
    }

    // 1 = got a line, 0 = timed out, -1 = closed and drained.
    int pop_timeout(std::string& out, int timeout_ms) {
        std::unique_lock<std::mutex> lock(mutex_);
        bool ready = cv_.wait_for(lock, std::chrono::milliseconds(timeout_ms),
                                  [&] { return !queue_.empty() || closed_; });
        if (!ready)
            return 0;
        if (queue_.empty())
            return -1;
        out = std::move(queue_.front());
        queue_.pop_front();
        return 1;
    }

    void close() {
        {
            std::lock_guard<std::mutex> lock(mutex_);
            closed_ = true;
        }
        cv_.notify_all();
    }

private:
    std::mutex mutex_;
    std::condition_variable cv_;
    std::deque<std::string> queue_;
    bool closed_ = false;
};

// std::cin replacement: underflow blocks until a command line is pushed to
// the in-queue, then serves it (with a restored '\n') as the get area.
// Stockfish reads commands with std::getline from a single thread (the UCI
// loop thread), so a single get-area buffer is sufficient.
class QueueInBuf : public std::streambuf {
public:
    explicit QueueInBuf(LineQueue& queue) : queue_(queue) {}

protected:
    int underflow() override {
        std::string line;
        if (!queue_.pop(line))
            return traits_type::eof();
        line_ = std::move(line);
        line_.push_back('\n');
        char* base = &line_[0];
        setg(base, base, base + line_.size());
        return traits_type::to_int_type(*gptr());
    }

private:
    LineQueue& queue_;
    std::string line_;
};

// std::cout replacement: accumulates characters and pushes a completed line
// to the out-queue on '\n'. Stockfish writes from multiple threads (search
// threads emit `info` lines); it guards whole lines with its sync_cout IO
// lock, but we do not rely solely on that: the partial-line accumulators are
// keyed by writer thread under our own mutex, so even interleaved writers
// cannot corrupt each other's lines.
class QueueOutBuf : public std::streambuf {
public:
    explicit QueueOutBuf(LineQueue& queue) : queue_(queue) {}

protected:
    int overflow(int ch) override {
        if (ch == traits_type::eof())
            return 0;
        std::lock_guard<std::mutex> lock(mutex_);
        put(static_cast<char>(ch));
        return ch;
    }

    std::streamsize xsputn(const char* s, std::streamsize n) override {
        std::lock_guard<std::mutex> lock(mutex_);
        for (std::streamsize i = 0; i < n; ++i)
            put(s[i]);
        return n;
    }

    int sync() override { return 0; }

private:
    // Caller must hold mutex_.
    void put(char ch) {
        std::string& partial = partials_[std::this_thread::get_id()];
        if (ch == '\n') {
            if (!partial.empty()) // never emit empty lines (sf_recv reserves 0 for timeout)
                queue_.push(std::move(partial));
            partial.clear();
        } else if (ch != '\r') {
            partial.push_back(ch);
        }
    }

    LineQueue& queue_;
    std::mutex mutex_;
    std::unordered_map<std::thread::id, std::string> partials_;
};

LineQueue g_to_engine;   // host -> engine command lines ("stdin")
LineQueue g_from_engine; // engine -> host output lines ("stdout")
std::atomic<bool> g_started{false};
std::atomic<bool> g_stop_called{false};
std::thread g_engine_thread;

} // namespace

extern "C" {

// Start the embedded engine. Returns 0 on success, nonzero if an instance
// was already started in this process (the global rdbuf redirection permits
// exactly one).
int32_t sf_start() {
    bool expected = false;
    if (!g_started.compare_exchange_strong(expected, true))
        return 1;

    // Leaked deliberately: the redirected rdbufs must outlive every iostream
    // user, including static destructors at process exit.
    std::cin.rdbuf(new QueueInBuf(g_to_engine));
    std::cout.rdbuf(new QueueOutBuf(g_from_engine));

    g_engine_thread = std::thread([] {
        // Replica of Stockfish 11's main() (vendor/stockfish/main.cpp),
        // minus taking over the process.
        std::cout << engine_info() << std::endl;

        UCI::init(Options);
        PSQT::init();
        Bitboards::init();
        Position::init();
        Bitbases::init();
        Endgames::init();
        Threads.set(size_t(Options["Threads"]));
        Search::clear(); // After threads are up

        char arg0[] = "stockfish";
        char* argv[] = {arg0, nullptr};
        UCI::loop(1, argv); // reads std::cin (our queue) until "quit"

        Threads.set(0);
        g_from_engine.close(); // sf_recv now reports engine-stopped (-1)
    });
    return 0;
}

// Push one command line (no trailing newline) to the engine's stdin queue.
void sf_send(const char* line) {
    if (line == nullptr)
        return;
    g_to_engine.push(std::string(line));
}

// Blocking pop of the next engine output line, with timeout. Copies at most
// cap-1 bytes into buf and NUL-terminates. Returns the line length
// (truncated to fit if needed), 0 on timeout, -1 once the engine has
// stopped and all output is drained.
int32_t sf_recv(char* buf, int32_t cap, int32_t timeout_ms) {
    if (buf == nullptr || cap <= 0)
        return -1;
    std::string line;
    int rc = g_from_engine.pop_timeout(line, timeout_ms);
    if (rc <= 0)
        return rc == 0 ? 0 : -1;
    size_t n = line.size();
    if (n > size_t(cap - 1))
        n = size_t(cap - 1);
    std::memcpy(buf, line.data(), n);
    buf[n] = '\0';
    return int32_t(n);
}

// Shut the engine down: send "quit", join the engine thread, close the
// queues. Idempotent; a no-op if the engine never started.
void sf_stop() {
    if (!g_started.load())
        return;
    if (g_stop_called.exchange(true))
        return;
    g_to_engine.push("quit");
    if (g_engine_thread.joinable())
        g_engine_thread.join();
    g_to_engine.close();
    g_from_engine.close();
}

} // extern "C"
