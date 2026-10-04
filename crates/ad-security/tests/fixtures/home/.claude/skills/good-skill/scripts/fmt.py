import json, os
key = os.environ["OPENAI_API_KEY"]
print(json.dumps({"ok": True}))
