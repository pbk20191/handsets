"""Reads an R8 mapping.txt and prints the obfuscated names the probe needs,
as one `key=kind:name` per line (m = method, m3 = method taking three
booleans, f = field, c = class).

    python probe_names.py mapping.txt

The probe gets them through `am instrument -e names "k=v;k=v;…"`. Without a
mapping (an unminified build) it uses the original names.
"""
import re
import sys

# logical key -> (class, candidate members; the first one present wins).
# Fields come first where there is one: R8 inlines plain getters and drops
# them. `getChildren$ui_release(boolean,boolean,boolean)` is what the public
# `children` getter calls; the probe passes (true, false, false).
WANT = {
    "view.semanticsOwner": ("androidx.compose.ui.platform.AndroidComposeView", ["semanticsOwner", "getSemanticsOwner()"]),
    "owner.unmergedRoot": ("androidx.compose.ui.semantics.SemanticsOwner", ["getUnmergedRootSemanticsNode()"]),
    "node.id": ("androidx.compose.ui.semantics.SemanticsNode", ["id", "getId()"]),
    "node.children": ("androidx.compose.ui.semantics.SemanticsNode",
                      ["getChildren()", "getChildren$ui_release(boolean,boolean,boolean)"]),
    "node.config": ("androidx.compose.ui.semantics.SemanticsNode", ["getConfig()", "unmergedConfig"]),
    "node.boundsInRoot": ("androidx.compose.ui.semantics.SemanticsNode", ["getBoundsInRoot()"]),
    "key.name": ("androidx.compose.ui.semantics.SemanticsPropertyKey", ["name", "getName()"]),
    "action.action": ("androidx.compose.ui.semantics.AccessibilityAction", ["action", "getAction()"]),
    "rect.left": ("androidx.compose.ui.geometry.Rect", ["left", "getLeft()"]),
    "rect.top": ("androidx.compose.ui.geometry.Rect", ["top", "getTop()"]),
    "rect.right": ("androidx.compose.ui.geometry.Rect", ["right", "getRight()"]),
    "rect.bottom": ("androidx.compose.ui.geometry.Rect", ["bottom", "getBottom()"]),
    "fn0.invoke": ("kotlin.jvm.functions.Function0", ["invoke()"]),
    "fn1.invoke": ("kotlin.jvm.functions.Function1", ["invoke(java.lang.Object)"]),
}
CLASSES = {
    "class.AccessibilityAction": "androidx.compose.ui.semantics.AccessibilityAction",
    "class.AnnotatedString": "androidx.compose.ui.text.AnnotatedString",
    "class.Unit": "kotlin.Unit",
}

CLASS_RE = re.compile(r"^(\S+) -> (\S+):$")
# "    1:5:Ret name(args):50:50 -> a"  (no range: an abstract method)
METHOD_RE = re.compile(r"^\s+(\d+:\d+:)?\S+ ([\w$<>.]+)\(([^)]*)\)(?::\d+(?::\d+)?)? -> (\S+)$")
# "    Type field -> b"
FIELD_RE = re.compile(r"^\s+\S+ ([\w$]+) -> (\S+)$")


def parse(path):
    """Classes and their surviving members. In R8's format, lines sharing a
    `start:end:` range and an obfuscated name are one position's inline
    stack, innermost first: only the last is the method that exists."""
    classes, members, cur, group = {}, {}, None, []

    def flush():
        if group:
            cls, key, obf = group[-1][1]
            if "." not in key.split("(")[0]:  # "Other.method" was inlined from elsewhere
                members.setdefault(cls, {}).setdefault(key, ("m", obf))
            group.clear()

    with open(path, encoding="utf-8") as f:
        for raw in f:
            line = raw.rstrip("\r\n")
            if not line or line.lstrip().startswith("#"):
                continue
            m = CLASS_RE.match(line)
            if m:
                flush()
                cur = m.group(1)
                classes[cur] = m.group(2)
                continue
            if cur is None:
                continue
            m = METHOD_RE.match(line)
            if m:
                rng, name, args, obf = m.groups()
                entry = (cur, f"{name}({args})", obf)
                if rng is None:
                    flush()
                    group.append(((None, obf), entry))
                    flush()
                    continue
                if group and group[-1][0] != (rng, obf):
                    flush()
                group.append(((rng, obf), entry))
                continue
            flush()
            m = FIELD_RE.match(line)
            if m:
                members.setdefault(cur, {}).setdefault(m.group(1), ("f", m.group(2)))
    flush()
    return classes, members


def main():
    classes, members = parse(sys.argv[1])
    for key, (cls, wants) in WANT.items():
        have = members.get(cls, {})
        want = next((w for w in wants if w in have), None)
        if want is None:
            print(f"# not in the mapping (kept as is, or removed): {key}", file=sys.stderr)
            continue
        kind, obf = have[want]
        if kind == "m" and want.endswith("(boolean,boolean,boolean)"):
            kind = "m3"
        print(f"{key}={kind}:{obf}")
    for key, cls in CLASSES.items():
        if cls in classes:
            print(f"{key}=c:{classes[cls]}")


if __name__ == "__main__":
    main()
