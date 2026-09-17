import { parse as parseToml } from "smol-toml";

const SNAPSHOT_KEY = "plugins/index.json";
const SNAPSHOT_CACHE_CONTROL = "public, max-age=300, s-maxage=1800, stale-while-revalidate=3600";
const GITHUB_QUERY = "topic:herdr-plugin is:public";
const GITHUB_API_VERSION = "2022-11-28";
const GITHUB_SEARCH_URL = "https://api.github.com/search/repositories";
const BLACKLIST_REPO_KEY_PREFIX = "repo:";
const PER_PAGE = 100;
const MAX_REPOS = 1000;
const REQUEST_TIMEOUT_MS = 10_000;
const GITHUB_API_URL = "https://api.github.com";
const GITHUB_RAW_URL = "https://raw.githubusercontent.com";
const MANIFEST_FILE_NAME = "herdr-plugin.toml";
const MANIFEST_MAX_BYTES = 32 * 1024;
const MAX_TREE_ENTRIES = 20_000;
const MAX_MANIFESTS_PER_REPOSITORY = 100;
const MAX_TOTAL_MANIFESTS = 5000;
const MAX_TOTAL_MANIFEST_BYTES = 16 * 1024 * 1024;
const PLUGIN_ID_MAX_CHARS = 120;
const PLUGIN_VERSION_MAX_CHARS = 64;

type R2Bucket = {
  put(
    key: string,
    value: string,
    options?: {
      httpMetadata?: {
        contentType?: string;
        cacheControl?: string;
      };
    },
  ): Promise<unknown>;
};

type KVNamespace = {
  list(options?: { prefix?: string; cursor?: string }): Promise<{
    keys: Array<{ name: string }>;
    cursor?: string;
  }>;
};

type ExecutionContext = {
  waitUntil(promise: Promise<unknown>): void;
};

type ScheduledController = unknown;

export type Env = {
  PLUGIN_MARKETPLACE_BUCKET: R2Bucket;
  PLUGIN_MARKETPLACE_BLACKLIST?: KVNamespace;
  GITHUB_TOKEN?: string;
};

type FetchLike = typeof fetch;

type RefreshOptions = {
  fetch?: FetchLike;
  now?: Date;
  logger?: Pick<Console, "error">;
};

type GitHubRepository = Record<string, unknown>;

export type PluginManifestEntry = {
  path: string;
  commit: string;
  id: string | null;
  name: string | null;
  version: string | null;
};

export type PluginListing = {
  id: number;
  fullName: string;
  owner: string;
  name: string;
  description: string | null;
  url: string;
  stars: number;
  forks: number;
  openIssues: number;
  language: string | null;
  topics: string[];
  createdAt: string | null;
  updatedAt: string | null;
  pushedAt: string | null;
  manifests?: PluginManifestEntry[];
};

export type PluginSnapshot = {
  schemaVersion: 1;
  generatedAt: string;
  source: {
    provider: "github";
    query: string;
    totalCount: number;
    collectedCount: number;
    truncated: boolean;
    warnings?: string[];
  };
  plugins: PluginListing[];
};

export type RefreshResult =
  | { ok: true; snapshot: PluginSnapshot }
  | { ok: false; error: string };

export default {
  fetch(): Response {
    return jsonResponse({ error: "Not found" }, 404, "no-store");
  },

  scheduled(_event: ScheduledController, env: Env, ctx: ExecutionContext): void {
    ctx.waitUntil(refreshPlugins(env));
  },
};

export async function refreshPlugins(
  env: Env,
  options: RefreshOptions = {},
): Promise<RefreshResult> {
  const logger = options.logger ?? console;
  try {
    const token = env.GITHUB_TOKEN?.trim();
    if (!token) {
      throw new Error("GITHUB_TOKEN is not configured");
    }

    const fetchFn = options.fetch ?? fetch;
    const result = await fetchGitHubRepositories(fetchFn, token);
    const normalizedPlugins = normalizeRepositories(result.repositories);
    if (normalizedPlugins.length === 0) {
      throw new Error("GitHub returned no listable plugin repositories");
    }

    const blockedRepositories = await readBlacklistedRepositories(env);
    const plugins =
      blockedRepositories.size === 0
        ? normalizedPlugins
        : normalizedPlugins.filter(
            (plugin) => !blockedRepositories.has(plugin.fullName.toLowerCase()),
          );

    const branchByRepo = defaultBranchByRepo(result.repositories);
    const manifestScan = await discoverManifests(fetchFn, token, plugins, branchByRepo);
    for (const plugin of plugins) {
      plugin.manifests = manifestScan.entries.get(plugin.fullName) ?? [];
    }

    const snapshot: PluginSnapshot = {
      schemaVersion: 1,
      generatedAt: (options.now ?? new Date()).toISOString(),
      source: {
        provider: "github",
        query: GITHUB_QUERY,
        totalCount: result.totalCount,
        collectedCount: result.repositories.length,
        truncated: result.truncated,
      },
      plugins,
    };

    if (result.truncated) {
      snapshot.source.warnings = [
        `GitHub returned ${result.totalCount} results; only the first ${result.repositories.length} were collected.`,
      ];
    }
    if (manifestScan.warnings.length > 0) {
      snapshot.source.warnings = [
        ...(snapshot.source.warnings ?? []),
        ...manifestScan.warnings,
      ];
    }

    await env.PLUGIN_MARKETPLACE_BUCKET.put(SNAPSHOT_KEY, JSON.stringify(snapshot), {
      httpMetadata: {
        contentType: "application/json; charset=utf-8",
        cacheControl: SNAPSHOT_CACHE_CONTROL,
      },
    });
    return { ok: true, snapshot };
  } catch (error) {
    const message = error instanceof Error ? error.message : "unknown refresh error";
    logger.error(`plugin marketplace refresh failed: ${message}`);
    return { ok: false, error: message };
  }
}

async function fetchGitHubRepositories(
  fetchFn: FetchLike,
  token: string,
  timeoutMs = REQUEST_TIMEOUT_MS,
): Promise<{ repositories: GitHubRepository[]; totalCount: number; truncated: boolean }> {
  const repositories: GitHubRepository[] = [];
  let totalCount = 0;

  for (let page = 1; repositories.length < MAX_REPOS; page += 1) {
    const url = new URL(GITHUB_SEARCH_URL);
    url.searchParams.set("q", GITHUB_QUERY);
    url.searchParams.set("per_page", String(PER_PAGE));
    url.searchParams.set("page", String(page));
    url.searchParams.set("sort", "stars");
    url.searchParams.set("order", "desc");

    const response = await fetchWithTimeout(
      fetchFn,
      url,
      {
        headers: {
          Accept: "application/vnd.github+json",
          Authorization: `Bearer ${token}`,
          "User-Agent": "herdr-plugin-marketplace",
          "X-GitHub-Api-Version": GITHUB_API_VERSION,
        },
      },
      timeoutMs,
    );

    if (!response.ok) {
      throw new Error(`GitHub search failed with status ${response.status}`);
    }

    const body = await response.json();
    if (!isObject(body) || typeof body.total_count !== "number" || !Array.isArray(body.items)) {
      throw new Error("GitHub search returned malformed JSON");
    }
    if (body.incomplete_results === true) {
      throw new Error("GitHub search returned incomplete results");
    }

    totalCount = body.total_count;
    repositories.push(...body.items.slice(0, MAX_REPOS - repositories.length));

    if (repositories.length >= totalCount || body.items.length === 0) {
      break;
    }
  }

  return {
    repositories,
    totalCount,
    truncated: totalCount > repositories.length,
  };
}

async function fetchWithTimeout(
  fetchFn: FetchLike,
  url: URL,
  init: RequestInit,
  timeoutMs: number,
): Promise<Response> {
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), timeoutMs);
  try {
    return await fetchFn(url, { ...init, signal: controller.signal });
  } finally {
    clearTimeout(timeout);
  }
}

export function normalizeRepositories(repositories: GitHubRepository[]): PluginListing[] {
  return repositories
    .map(normalizeRepository)
    .filter((plugin): plugin is PluginListing => plugin !== null)
    .sort(comparePlugins);
}

function defaultBranchByRepo(repositories: GitHubRepository[]): Map<string, string> {
  const branches = new Map<string, string>();
  for (const repo of repositories) {
    const fullName = readString(repo.full_name);
    const branch = readString(repo.default_branch);
    if (fullName && branch) {
      branches.set(fullName, branch);
    }
  }
  return branches;
}

type ManifestScan = {
  entries: Map<string, PluginManifestEntry[]>;
  warnings: string[];
};

async function discoverManifests(
  fetchFn: FetchLike,
  token: string,
  plugins: PluginListing[],
  branchByRepo: Map<string, string>,
): Promise<ManifestScan> {
  const entries = new Map<string, PluginManifestEntry[]>();
  const warnings: string[] = [];
  let totalManifests = 0;
  let totalBytes = 0;

  for (const plugin of plugins) {
    const branch = branchByRepo.get(plugin.fullName);
    if (!branch) {
      continue;
    }
    if (totalManifests >= MAX_TOTAL_MANIFESTS || totalBytes >= MAX_TOTAL_MANIFEST_BYTES) {
      warnings.push(
        `Manifest discovery stopped early at ${plugin.fullName}: global caps reached.`,
      );
      break;
    }
    const headCommit = await fetchHeadCommit(fetchFn, token, plugin, branch);
    if (!headCommit) {
      warnings.push(`Skipped manifest scan for ${plugin.fullName}: head commit lookup failed.`);
      continue;
    }
    const paths = await fetchManifestPaths(fetchFn, token, plugin, branch);
    if (!paths) {
      warnings.push(`Skipped manifest scan for ${plugin.fullName}: file tree unavailable.`);
      continue;
    }
    if (paths.length > MAX_MANIFESTS_PER_REPOSITORY) {
      warnings.push(
        `${plugin.fullName} was skipped because it contains more than ${MAX_MANIFESTS_PER_REPOSITORY} plugin manifests.`,
      );
      continue;
    }
    const found: PluginManifestEntry[] = [];
    for (const path of paths) {
      if (totalManifests >= MAX_TOTAL_MANIFESTS || totalBytes >= MAX_TOTAL_MANIFEST_BYTES) {
        break;
      }
      const text = await fetchManifestText(fetchFn, token, plugin, branch, path);
      if (text === null) {
        continue;
      }
      totalBytes += text.length;
      totalManifests += 1;
      const metadata = readManifestMetadata(text);
      found.push({ path, commit: headCommit, ...metadata });
    }
    found.sort((a, b) => a.path.localeCompare(b.path));
    entries.set(plugin.fullName, found);
  }
  return { entries, warnings };
}

async function githubApi(
  fetchFn: FetchLike,
  token: string,
  url: URL,
  timeoutMs = REQUEST_TIMEOUT_MS,
): Promise<unknown | null> {
  const response = await fetchWithTimeout(
    fetchFn,
    url,
    {
      headers: {
        Accept: "application/vnd.github+json",
        Authorization: `Bearer ${token}`,
        "User-Agent": "herdr-plugin-marketplace",
        "X-GitHub-Api-Version": GITHUB_API_VERSION,
      },
    },
    timeoutMs,
  );
  if (!response.ok) {
    return null;
  }
  try {
    return await response.json();
  } catch {
    return null;
  }
}

async function fetchHeadCommit(
  fetchFn: FetchLike,
  token: string,
  plugin: PluginListing,
  branch: string,
): Promise<string | null> {
  const url = new URL(
    `${GITHUB_API_URL}/repos/${encodeURIComponent(plugin.owner)}/${encodeURIComponent(plugin.name)}/commits/${encodeURIComponent(branch)}`,
  );
  url.searchParams.set("per_page", "1");
  const body = await githubApi(fetchFn, token, url);
  if (!isObject(body)) {
    return null;
  }
  const sha = readString(body.sha);
  return sha && /^[0-9a-f]{40}$/.test(sha) ? sha : null;
}

async function fetchManifestPaths(
  fetchFn: FetchLike,
  token: string,
  plugin: PluginListing,
  branch: string,
): Promise<string[] | null> {
  const url = new URL(
    `${GITHUB_API_URL}/repos/${encodeURIComponent(plugin.owner)}/${encodeURIComponent(plugin.name)}/git/trees/${encodeURIComponent(branch)}`,
  );
  url.searchParams.set("recursive", "1");
  const body = await githubApi(fetchFn, token, url);
  if (!isObject(body) || !Array.isArray(body.tree)) {
    return null;
  }
  if (body.truncated === true || body.tree.length > MAX_TREE_ENTRIES) {
    return null;
  }
  const paths: string[] = [];
  for (const entry of body.tree) {
    if (!isObject(entry) || entry.type !== "blob") {
      continue;
    }
    const path = readString(entry.path);
    if (path && (path === MANIFEST_FILE_NAME || path.endsWith(`/${MANIFEST_FILE_NAME}`))) {
      paths.push(path);
    }
  }
  return paths;
}

async function fetchManifestText(
  fetchFn: FetchLike,
  token: string,
  plugin: PluginListing,
  branch: string,
  path: string,
): Promise<string | null> {
  const url = new URL(
    `${GITHUB_RAW_URL}/${encodeURIComponent(plugin.owner)}/${encodeURIComponent(plugin.name)}/${encodeURIComponent(branch)}/${path
      .split("/")
      .map(encodeURIComponent)
      .join("/")}`,
  );
  const response = await fetchWithTimeout(
    fetchFn,
    url,
    {
      headers: {
        Accept: "text/plain",
        Authorization: `Bearer ${token}`,
        "User-Agent": "herdr-plugin-marketplace",
      },
    },
    REQUEST_TIMEOUT_MS,
  );
  if (!response.ok) {
    return null;
  }
  const text = await response.text();
  if (text.length > MANIFEST_MAX_BYTES) {
    return null;
  }
  return text;
}

function readManifestMetadata(text: string): {
  id: string | null;
  name: string | null;
  version: string | null;
} {
  let parsed: unknown;
  try {
    parsed = parseToml(text.replace(/^\uFEFF/, ""));
  } catch {
    return { id: null, name: null, version: null };
  }
  if (!isObject(parsed)) {
    return { id: null, name: null, version: null };
  }
  const id = readCappedString(parsed.id, PLUGIN_ID_MAX_CHARS);
  const name = readCappedString(parsed.name, PLUGIN_ID_MAX_CHARS);
  const version = readCappedString(parsed.version, PLUGIN_VERSION_MAX_CHARS);
  return { id, name, version };
}

function readCappedString(value: unknown, maxChars: number): string | null {
  if (typeof value !== "string" || value.length === 0 || value.length > maxChars) {
    return null;
  }
  return value;
}

async function readBlacklistedRepositories(env: Env): Promise<Set<string>> {
  const kv = env.PLUGIN_MARKETPLACE_BLACKLIST;
  const blockedRepositories = new Set<string>();
  if (!kv) {
    return blockedRepositories;
  }

  let cursor: string | undefined;
  do {
    const page = await kv.list({ prefix: BLACKLIST_REPO_KEY_PREFIX, cursor });
    for (const key of page.keys) {
      const repository = key.name.slice(BLACKLIST_REPO_KEY_PREFIX.length).trim().toLowerCase();
      if (repository.includes("/")) {
        blockedRepositories.add(repository);
      }
    }
    cursor = page.cursor;
  } while (cursor);

  return blockedRepositories;
}

function normalizeRepository(repo: GitHubRepository): PluginListing | null {
  if (
    readBoolean(repo.disabled) ||
    readBoolean(repo.archived) ||
    readBoolean(repo.fork) ||
    readBoolean(repo.private) ||
    readString(repo.visibility) === "private"
  ) {
    return null;
  }

  const fullNameParts = splitFullName(readString(repo.full_name));
  const ownerObject = isObject(repo.owner) ? repo.owner : {};
  const owner = firstString(readString(ownerObject.login), fullNameParts.owner);
  const name = firstString(readString(repo.name), fullNameParts.name);
  if (!owner || !name) {
    return null;
  }

  const fullName = readString(repo.full_name) ?? `${owner}/${name}`;
  const url = readString(repo.html_url);
  if (!url || !isValidGitHubRepoUrl(url, owner, name)) {
    return null;
  }

  return {
    id: readInteger(repo.id) ?? 0,
    fullName,
    owner,
    name,
    description: readNullableString(repo.description),
    url,
    stars: readNonNegativeInteger(repo.stargazers_count),
    forks: readNonNegativeInteger(repo.forks_count),
    openIssues: readNonNegativeInteger(repo.open_issues_count),
    language: readNullableString(repo.language),
    topics: readStringArray(repo.topics),
    createdAt: readIsoString(repo.created_at),
    updatedAt: readIsoString(repo.updated_at),
    pushedAt: readIsoString(repo.pushed_at),
  };
}

function comparePlugins(a: PluginListing, b: PluginListing): number {
  return (
    b.stars - a.stars ||
    dateMs(b.pushedAt) - dateMs(a.pushedAt) ||
    a.fullName.localeCompare(b.fullName)
  );
}

function jsonResponse(body: unknown, status: number, cacheControl: string): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: {
      "Content-Type": "application/json; charset=utf-8",
      "Cache-Control": cacheControl,
    },
  });
}

function isValidGitHubRepoUrl(url: string, owner: string, name: string): boolean {
  try {
    const parsed = new URL(url);
    const segments = parsed.pathname.split("/").filter(Boolean);
    return (
      parsed.protocol === "https:" &&
      parsed.hostname === "github.com" &&
      segments.length === 2 &&
      segments[0].toLowerCase() === owner.toLowerCase() &&
      segments[1].toLowerCase() === name.toLowerCase()
    );
  } catch {
    return false;
  }
}

function splitFullName(fullName: string | null): { owner: string | null; name: string | null } {
  if (!fullName) {
    return { owner: null, name: null };
  }
  const [owner, name, extra] = fullName.split("/");
  if (!owner || !name || extra) {
    return { owner: null, name: null };
  }
  return { owner, name };
}

function firstString(...values: Array<string | null>): string | null {
  return values.find((value) => value !== null) ?? null;
}

function readString(value: unknown): string | null {
  return typeof value === "string" && value.length > 0 ? value : null;
}

function readNullableString(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

function readStringArray(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((item): item is string => typeof item === "string") : [];
}

function readInteger(value: unknown): number | null {
  return typeof value === "number" && Number.isInteger(value) ? value : null;
}

function readNonNegativeInteger(value: unknown): number {
  const integer = readInteger(value);
  return integer !== null && integer >= 0 ? integer : 0;
}

function readBoolean(value: unknown): boolean {
  return value === true;
}

function readIsoString(value: unknown): string | null {
  if (typeof value !== "string") {
    return null;
  }
  return Number.isNaN(Date.parse(value)) ? null : value;
}

function dateMs(value: string | null): number {
  return value ? Date.parse(value) || 0 : 0;
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}
