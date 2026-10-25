# 量 agent 文案的 token 开销（cl100k_base 近似）。
# 用法:
#   python tools/token_cmp.py                 # 只量源码里的 workflow_text
#   python tools/token_cmp.py <文案文件>      # 量一份真实生成的文案
import re
import sys

import tiktoken

enc = tiktoken.get_encoding("cl100k_base")


def n(s):
    return len(enc.encode(s))


if len(sys.argv) > 1:
    text = open(sys.argv[1], encoding="utf-8").read()
    cjk = len(re.findall(r"[\u4e00-\u9fff]", text))
    print(f"文案: {len(text)} 字符（中文 {cjk}） -> {n(text)} token")
    sys.exit(0)

src = open("src/ops.rs", encoding="utf-8").read()
m = re.search(r"fn workflow_text\(.*?\n\}\n", src, re.S)
body = m.group(0)
lits = re.findall(r'"((?:[^"\\]|\\.)*)"', body)
zh = "\n".join(l for l in lits if re.search(r"[\u4e00-\u9fff]", l))
zh = zh.replace("\\n", "\n").replace('\\"', '"')

# 英文版：全部字符串字面量拼起来（命令模板 + 说明）
en = "\n".join(l for l in lits if l.strip())
en = en.replace("\\n", "\n").replace('\\"', '"')
en = re.sub(r"\{[a-z_0-9]+\}", "X", en)  # 占位符统一，量级稳定

print(f"workflow_text 英文速查表: {len(en)} 字符 -> {n(en)} token")
print(f"其中中文字符串（阶段名等）: {len(zh)} 字符 -> {n(zh)} token")
print(f"改造前同义中文指令实测 ~882 token；现在 {n(en)} token")
