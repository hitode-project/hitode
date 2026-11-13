import assert from "node:assert/strict";
import test from "node:test";
import { createHostBridge, isAclError, type HostInfo } from "./bridge.ts";

const ANDROID_INFO: HostInfo = {
  version: "0.2.7",
  platform: "android",
  gameView: true,
};

test("isAclError matches release and debug Tauri origin rejections only", () => {
  assert.equal(isAclError("Command host_info not allowed by ACL"), true);
  assert.equal(
    isAclError('event.listen not allowed on window "main", webview "main", URL: about:blank'),
    true,
  );
  assert.equal(isAclError("host_info not allowed. Plugin not found"), true);
  assert.equal(isAclError("opener.open_url not allowed on origin [remote: about:blank]"), true);
  assert.equal(isAclError("no game is running"), false);
  assert.equal(isAclError(new Error("No folder selected")), false);
});

test("startup ACL rejections retry until the webview URL settles, then real ACL errors fail at once", async () => {
  let urlReady = false;
  const delays: number[] = [];
  let calls = 0;

  const bridge = createHostBridge({
    inTauri: true,
    wait: async (ms) => {
      delays.push(ms);
      if (delays.length >= 2) urlReady = true;
    },
    invoke: async <T>(command: string): Promise<T> => {
      calls += 1;
      if (!urlReady) throw "Command " + command + " not allowed by ACL";
      if (command === "host_info") return ANDROID_INFO as T;
      throw "Command " + command + " not allowed by ACL";
    },
    listen: async () => () => {},
  });

  const info = await bridge.hostInfo();
  assert.deepEqual(info, ANDROID_INFO);
  assert.equal(calls, 3);
  assert.deepEqual(delays, [16, 32]);

  // Once IPC has succeeded, a genuine ACL failure rejects without retrying.
  await assert.rejects(
    bridge.invokeHost("plugin:opener|open_url", { url: "https://example.com" }),
    /Command plugin:opener\|open_url not allowed by ACL/,
  );
  assert.equal(calls, 4);
  assert.deepEqual(delays, [16, 32]);
});

test("non-ACL runtime errors are never retried during startup", async () => {
  let calls = 0;
  const delays: number[] = [];

  const bridge = createHostBridge({
    inTauri: true,
    wait: async (ms) => {
      delays.push(ms);
    },
    invoke: async () => {
      calls += 1;
      throw new Error("corrupt library");
    },
    listen: async () => () => {},
  });

  await assert.rejects(bridge.call("library_list", {}, []), /corrupt library/);
  assert.equal(calls, 1);
  assert.deepEqual(delays, []);
});

test("hostInfo deduplicates concurrent calls, caches fulfillment, and never caches rejection", async () => {
  let shouldFail = true;
  let calls = 0;

  const bridge = createHostBridge({
    inTauri: true,
    wait: async () => {},
    invoke: async <T>(): Promise<T> => {
      calls += 1;
      if (shouldFail) throw new Error("transient failure");
      return ANDROID_INFO as T;
    },
    listen: async () => () => {},
  });

  await assert.rejects(bridge.hostInfo(), /transient failure/);
  assert.equal(calls, 1);

  shouldFail = false;
  const [first, second] = await Promise.all([bridge.hostInfo(), bridge.hostInfo()]);
  assert.deepEqual(first, ANDROID_INFO);
  assert.deepEqual(second, ANDROID_INFO);
  assert.equal(calls, 2);

  const cached = await bridge.hostInfo();
  assert.deepEqual(cached, ANDROID_INFO);
  assert.equal(calls, 2);
});

test("onHostEvent retries startup ACL rejections, cancels cleanly, and reports permanent failures", async () => {
  let listenAttempts = 0;
  let emit: ((payload: string) => void) | null = null;
  let stopped = 0;
  const errors: [string, unknown][] = [];

  const bridge = createHostBridge({
    inTauri: true,
    wait: async () => {},
    onError: (message, error) => {
      errors.push([message, error]);
    },
    invoke: async <T>(): Promise<T> => {
      throw "Command host_info not allowed by ACL";
    },
    listen: async <T>(name: string, handler: (payload: T) => void) => {
      listenAttempts += 1;
      if (name === "broken") throw "Command plugin:event|listen not allowed by ACL";
      if (listenAttempts < 3) throw "Command plugin:event|listen not allowed by ACL";
      emit = handler as unknown as (payload: string) => void;
      return () => {
        stopped += 1;
      };
    },
  });

  // Dropping before retry completes stops retrying and logs nothing.
  let cancelledCalls = 0;
  const cancelledBridge = createHostBridge({
    inTauri: true,
    wait: async () => {
      dropEarly();
    },
    onError: (message, error) => {
      errors.push([message, error]);
    },
    invoke: async <T>(): Promise<T> => ANDROID_INFO as T,
    listen: async () => {
      cancelledCalls += 1;
      throw "Command plugin:event|listen not allowed by ACL";
    },
  });
  let dropEarly = () => {};
  dropEarly = cancelledBridge.onHostEvent("edge_back", () => {});
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(cancelledCalls, 1);
  assert.deepEqual(errors, []);

  // Permanent failure logs via onError instead of throwing an unhandled rejection.
  const brokenBridge = createHostBridge({
    inTauri: true,
    wait: async () => {},
    onError: (message, error) => {
      errors.push([message, error]);
    },
    invoke: async <T>(): Promise<T> => ANDROID_INFO as T,
    listen: async () => {
      throw "Command plugin:event|listen not allowed by ACL";
    },
  });
  brokenBridge.onHostEvent("broken", () => {});
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(errors.length, 1);
  assert.equal(errors[0]?.[0], "listen(broken) failed");

  // Startup retry succeeds and delivers events until unsubscribed.
  const received: string[] = [];
  const stop = bridge.onHostEvent<string>("game_finished", (payload) => received.push(payload));
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(listenAttempts, 3);
  emit?.("g1");
  stop();
  emit?.("g2");
  assert.deepEqual(received, ["g1"]);
  assert.equal(stopped, 1);
});
