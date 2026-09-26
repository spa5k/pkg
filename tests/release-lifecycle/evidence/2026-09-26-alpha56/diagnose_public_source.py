#!/usr/bin/env python3
"""Record anonymous GitHub status fields on a disposable hosted runner."""
import json
import os
import urllib.error
import urllib.request

if os.environ.get('GITHUB_ACTIONS') != 'true' or os.environ.get('RUNNER_ENVIRONMENT') != 'github-hosted':
    raise SystemExit('requires a disposable GitHub-hosted runner')

url = 'https://api.github.com/repos/casey/just/commits/HEAD'
request = urllib.request.Request(url, headers={'User-Agent': 'pkg-release-diagnostics', 'Accept': 'application/vnd.github+json'})
try:
    response = urllib.request.urlopen(request, timeout=30)
except urllib.error.HTTPError as error:
    response = error
except Exception as error:
    print(json.dumps({'url': url, 'errorType': type(error).__name__}))
    raise SystemExit(0)
with response:
    body = response.read(1024 * 1024)
    record = {'url': url, 'status': response.status, 'headers': {
        name: response.headers.get(name) for name in
        ['x-ratelimit-limit', 'x-ratelimit-remaining', 'x-ratelimit-reset', 'x-ratelimit-resource', 'retry-after']}}
    try:
        value = json.loads(body)
        record['sha'] = value.get('sha')
        record['message'] = value.get('message')
    except (ValueError, AttributeError):
        record['bodyFormat'] = 'not a JSON object'
    print(json.dumps(record, indent=2))
