#define _POSIX_C_SOURCE 200809L
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <termios.h>
#include <time.h>
#include <unistd.h>

/* A real terminal peer, built with the C toolchain already required by Herdr.
   Reports go through the real CLI and isolated server, like upstream's shell probes. */
static FILE *log_file;
static const char *herdr, *pane, *agent;
static int reporting;

static void json_string(const unsigned char *s, size_t n) {
    fputc('"', log_file);
    for (size_t i = 0; i < n; ++i) {
        if (s[i] == '"' || s[i] == '\\') fprintf(log_file, "\\%c", s[i]);
        else if (s[i] < 32) fprintf(log_file, "\\u%04x", s[i]);
        else fputc(s[i], log_file);
    }
    fputc('"', log_file);
}

static void record_text(const char *kind, const unsigned char *text, size_t n) {
    fprintf(log_file, "{\"kind\":\"%s\",\"text\":", kind);
    json_string(text, n);
    fputs("}\n", log_file);
    fflush(log_file);
}

static int report(const char *state, const char *title, int session) {
    if (!reporting) return 0;
    pid_t pid = fork();
    if (pid < 0) return -1;
    if (pid == 0) {
        int null = open("/dev/null", O_WRONLY);
        if (null >= 0) { dup2(null, STDOUT_FILENO); close(null); }
        if (session) execl(herdr, herdr, "pane", "report-agent-session", pane, "--source", "prompt-parity", "--agent", agent, "--agent-session-id", "replacement", (char *)NULL);
        else if (title) execl(herdr, herdr, "pane", "report-metadata", pane, "--source", "prompt-parity", "--agent", agent, "--title", title, (char *)NULL);
        else execl(herdr, herdr, "pane", "report-agent", pane, "--source", "prompt-parity", "--agent", agent, "--state", state, (char *)NULL);
        _exit(127);
    }
    int status;
    while (waitpid(pid, &status, 0) < 0) if (errno != EINTR) return -1;
    return WIFEXITED(status) && WEXITSTATUS(status) == 0 ? 0 : -1;
}

static int submitted(const unsigned char *text, size_t n) {
    record_text("submission", text, n);
    if (!reporting || strcmp((const char *)text, "do not transition") == 0) return 0;
    if (strcmp((const char *)text, "exit after submit") == 0) exit(0);
    if (strcmp((const char *)text, "session churn") == 0) return report(NULL, NULL, 1);
    if (strcmp((const char *)text, "done churn") == 0) {
        if (report("idle", "done churn", 0)) return -1;
        return report("idle", "ready", 0);
    }
    if (strcmp((const char *)text, "block after submit") == 0) return report("blocked", NULL, 0);
    if (report("working", NULL, 0)) return -1;
    return report("idle", NULL, 0);
}

int main(int argc, char **argv) {
    if (argc != 7) return 2; /* log, socket, pane, agent, raw|bracketed, herdr */
    log_file = fopen(argv[1], "a");
    if (!log_file) return 3;
    reporting = strcmp(argv[2], "-") != 0;
    if (reporting && setenv("HERDR_SOCKET_PATH", argv[2], 1)) return 4;
    pane = argv[3]; agent = argv[4]; herdr = argv[6];
    struct termios raw;
    if (tcgetattr(STDIN_FILENO, &raw)) return 5;
    raw.c_iflag &= ~(IGNBRK | BRKINT | PARMRK | ISTRIP | INLCR | IGNCR | ICRNL | IXON);
    raw.c_oflag &= ~OPOST;
    raw.c_lflag &= ~(ECHO | ECHONL | ICANON | ISIG | IEXTEN);
    raw.c_cflag = (raw.c_cflag & ~(CSIZE | PARENB)) | CS8;
    raw.c_cc[VMIN] = 1; raw.c_cc[VTIME] = 0;
    if (tcsetattr(STDIN_FILENO, TCSANOW, &raw)) return 6;
    fputs(strcmp(argv[5], "raw") == 0 ? "\033[?2004l" : "\033[?2004h", stdout);
    fputs("PROMPT_AGENT_READY\r\n", stdout); fflush(stdout);
    fputs("{\"kind\":\"ready\"}\n", log_file); fflush(log_file);
    unsigned char *pending = NULL; size_t len = 0;
    for (;;) {
        unsigned char chunk[8192];
        ssize_t count = read(STDIN_FILENO, chunk, sizeof chunk);
        if (count < 0 && errno == EINTR) continue;
        if (count <= 0) break;
        struct timespec now; clock_gettime(CLOCK_MONOTONIC, &now);
        fprintf(log_file, "{\"kind\":\"bytes\",\"time_ns\":\"%lld\",\"hex\":\"", (long long)now.tv_sec * 1000000000LL + now.tv_nsec);
        for (ssize_t i = 0; i < count; ++i) fprintf(log_file, "%02x", chunk[i]);
        fputs("\"}\n", log_file); fflush(log_file);
        unsigned char *next = realloc(pending, len + (size_t)count + 1);
        if (!next) return 7;
        pending = next; memcpy(pending + len, chunk, (size_t)count); len += (size_t)count; pending[len] = 0;
        while (len) {
            if (len >= 3 && memcmp(pending, "\033[I", 3) == 0) {
                memmove(pending, pending + 3, len - 3); len -= 3; pending[len] = 0; continue;
            }
            size_t start = 0, end, consumed;
            if (len >= 6 && memcmp(pending, "\033[200~", 6) == 0) {
                unsigned char *paste_end = (unsigned char *)strstr((char *)pending + 6, "\033[201~");
                if (!paste_end) break;
                end = (size_t)(paste_end - pending); start = 6; consumed = end + 7;
                if (len < consumed) break;
                if (pending[consumed - 1] != '\r') return 8;
            } else {
                unsigned char *enter = memchr(pending, '\r', len);
                if (!enter) break;
                end = (size_t)(enter - pending); consumed = end + 1;
            }
            unsigned char *text = malloc(end - start + 1);
            if (!text) return 9;
            memcpy(text, pending + start, end - start); text[end - start] = 0;
            if (submitted(text, end - start)) {
                record_text("error", (const unsigned char *)"agent report failed", 19); free(text); return 10;
            }
            record_text("settled", text, end - start); free(text);
            memmove(pending, pending + consumed, len - consumed); len -= consumed; pending[len] = 0;
        }
    }
    free(pending); fclose(log_file); return 0;
}
