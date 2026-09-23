use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use chromiumoxide::page::ScreenshotParams;
use chromiumoxide::{Browser, BrowserConfig, Page};
use futures::StreamExt;
use munin_types::DOMElementNode;
use std::sync::Arc;

use crate::BrowserDriver;

/// 基于 Chrome DevTools Protocol (CDP) 的底层浏览器驱动
pub struct CdpDriver {
    browser: Option<Arc<Browser>>,
    page: Option<Page>,
    _handler_task: Option<tokio::task::JoinHandle<()>>,
    /// 自启动实例的专属临时 profile 目录（connect 模式为 None），Drop 时清理
    owned_data_dir: Option<std::path::PathBuf>,
}

impl Drop for CdpDriver {
    fn drop(&mut self) {
        if let Some(dir) = self.owned_data_dir.take() {
            // 浏览器进程由 chromiumoxide 在 Browser drop 时终止；此处仅清理 profile 目录
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

impl CdpDriver {
    /// 连接到已运行的 Chromium/Chrome 实例（如 http://127.0.0.1:9222）
    pub async fn connect(url: impl Into<String>) -> Result<Self> {
        let endpoint = url.into();
        let (browser, mut handler) = Browser::connect(&endpoint)
            .await
            .with_context(|| format!("Failed to connect to CDP at {}", endpoint))?;

        let handler_task = tokio::spawn(async move {
            while let Some(_event) = handler.next().await {}
        });

        let page = browser.new_page("about:blank").await?;
        Ok(Self {
            browser: Some(Arc::new(browser)),
            page: Some(page),
            _handler_task: Some(handler_task),
            owned_data_dir: None,
        })
    }

    /// 启动本地 Chromium/Chrome 实例
    pub async fn launch_headless(headless: bool) -> Result<Self> {
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let data_dir =
            std::env::temp_dir().join(format!("munin-chrome-{}-{}", std::process::id(), millis));
        std::fs::create_dir_all(&data_dir)?;
        let mut builder = BrowserConfig::builder()
            .viewport(None)
            .window_size(1920, 1080)
            .arg("--start-maximized")
            .arg("--no-default-browser-check")
            .user_data_dir(&data_dir);
        if !headless {
            builder = builder.with_head();
        }
        let config = builder
            .build()
            .map_err(|e| anyhow!("Failed to build browser config: {e}"))?;

        let (browser, mut handler) = Browser::launch(config)
            .await
            .context("Failed to launch chromium instance")?;

        let handler_task = tokio::spawn(async move {
            while let Some(_event) = handler.next().await {}
        });

        let page = browser.new_page("about:blank").await?;
        Ok(Self {
            browser: Some(Arc::new(browser)),
            page: Some(page),
            _handler_task: Some(handler_task),
            owned_data_dir: Some(data_dir),
        })
    }

    /// 优先连接已有调试端口，若不可用则启动新无头浏览器
    pub async fn connect_or_launch(addr_or_url: &str) -> Result<Self> {
        match Self::connect(addr_or_url).await {
            Ok(driver) => Ok(driver),
            Err(e) => {
                tracing::warn!(
                    "CDP 调试端口连接失败 ({:?})，尝试自启动本地无头浏览器...",
                    e
                );
                Self::launch_headless(true).await
            }
        }
    }

    fn active_page(&self) -> Result<&Page> {
        self.page
            .as_ref()
            .ok_or_else(|| anyhow!("Browser page is not initialized. Call launch or goto first."))
    }
}

#[async_trait]
impl BrowserDriver for CdpDriver {
    async fn launch(&mut self, headless: bool) -> Result<()> {
        if self.browser.is_none() {
            let mut instance = Self::launch_headless(headless).await?;
            self.browser = instance.browser.take();
            self.page = instance.page.take();
            self._handler_task = instance._handler_task.take();
            // take 后 instance.owned_data_dir 仍持有目录，手动转移到 self 防止 Drop 清理
            self.owned_data_dir = instance.owned_data_dir.take();
        }
        Ok(())
    }

    async fn goto(&mut self, url: &str) -> Result<()> {
        if self.page.is_none() {
            self.launch(true).await?;
        }
        let page = self.active_page()?;
        page.goto(url).await?;
        Ok(())
    }

    async fn get_interactive_elements(&self) -> Result<Vec<DOMElementNode>> {
        let page = self.active_page()?;
        let script = r#"
            (() => {
                const selector = 'button, a, input, select, textarea, [role="button"], [role="link"], [onclick]';
                const elements = Array.from(document.querySelectorAll(selector));
                // 活动图层检测：aria-modal / role=dialog / AntD / Element 弹层与 Portal 浮层
                const layerSel = '[aria-modal="true"], [role="dialog"], .ant-modal-wrap, .ant-drawer-open, .ant-select-dropdown, .ant-picker-dropdown, .ant-popconfirm, .ant-popover, .ant-tooltip, .el-dialog__wrapper, .el-drawer__wrapper, .el-select-dropdown, .el-popper';
                const zOf = (l) => {
                    const z = parseInt(getComputedStyle(l).zIndex, 10);
                    return isNaN(z) ? 0 : z;
                };
                const activeLayers = Array.from(document.querySelectorAll(layerSel))
                    .filter(l => {
                        const s = getComputedStyle(l);
                        return s.display !== 'none' && s.visibility !== 'hidden';
                    });
                // 多层弹窗叠加时只认最高 z-index 层（及并列），其余视为被遮罩
                const topZ = activeLayers.length > 0 ? Math.max(...activeLayers.map(zOf)) : 0;
                const topLayers = activeLayers.filter(l => zOf(l) === topZ);
                const inTopLayer = (el) => topLayers.some(l => l.contains(el) || l === el);
                const layerActive = activeLayers.length > 0;
                return elements.map((el, idx) => {
                    let id = el.getAttribute('data-munin-id') || el.id;
                    if (!id) {
                        id = 'munin-node-' + idx;
                        el.setAttribute('data-munin-id', id);
                    }
                    const attrs = {};
                    for (const attr of el.attributes) {
                        attrs[attr.name] = attr.value;
                    }
                    const rawText = (el.innerText || el.textContent || el.value || el.placeholder || '').trim();
                    // CJK 双汉字按钮自动插空格（"确 定"），归一化副本供模糊匹配
                    attrs['data-munin-text-normalized'] = rawText.replace(/\s+/g, '');
                    // 图层标记：仅最顶层弹窗内元素 active；其余（含低层弹窗）全部 buried
                    attrs['data-munin-layer'] = inTopLayer(el) ? 'active' : (layerActive ? 'buried' : 'normal');
                    // 指纹注册表：Vue 重渲染销毁节点后，click/fill 可按稳定属性回退重定位
                    (window.__munin_registry = window.__munin_registry || {})[id] = {
                        tag: el.tagName.toLowerCase(),
                        text: rawText.replace(/\s+/g, ''),
                        id_attr: el.id || '',
                        name: el.getAttribute('name') || '',
                        aria: el.getAttribute('aria-label') || '',
                        placeholder: el.getAttribute('placeholder') || '',
                        type: el.getAttribute('type') || ''
                    };
                    return {
                        node_id: id,
                        tag: el.tagName.toLowerCase(),
                        text: rawText,
                        attributes: attrs
                    };
                });
            })()
        "#;

        let eval_fut = page.evaluate(script);
        let eval_res = tokio::time::timeout(std::time::Duration::from_secs(3), eval_fut)
            .await
            .map_err(|_| anyhow!("CDP evaluate timed out waiting for execution context"))??;
        let value = eval_res.into_value::<serde_json::Value>()?;
        let nodes: Vec<DOMElementNode> = serde_json::from_value(value)?;
        Ok(nodes)
    }

    async fn click(&self, node_id: &str) -> Result<()> {
        // 防止抓取幽灵节点：等待弹窗/气泡 CSS 退出动画结束
        let _ = self.wait_for_stable().await;
        let page = self.active_page()?;
        let escaped_id = serde_json::to_string(node_id)?;
        let script = format!(
            r#"
            (() => {{
                const id = {escaped_id};
                let el = document.querySelector('[data-munin-id="' + id + '"]') || document.getElementById(id);
                if (!el) {{
                    // 防漂移回退：按注册表指纹（tag + id/name/aria/placeholder/归一化文本）重定位
                    const fp = (window.__munin_registry || {{}})[id];
                    if (fp) {{
                        const cand = Array.from(document.querySelectorAll(fp.tag));
                        let best = null, bestScore = 0;
                        for (const c of cand) {{
                            let s = 0;
                            if (fp.id_attr && c.id === fp.id_attr) s += 100;
                            if (fp.name && c.getAttribute('name') === fp.name) s += 50;
                            if (fp.aria && c.getAttribute('aria-label') === fp.aria) s += 40;
                            if (fp.placeholder && c.getAttribute('placeholder') === fp.placeholder) s += 40;
                            if (fp.text) {{
                                const ct = (c.innerText || c.textContent || c.value || '').replace(/\s+/g, '');
                                if (ct === fp.text) s += 30;
                                else if (ct && fp.text && (ct.includes(fp.text) || fp.text.includes(ct))) s += 15;
                            }}
                            if (s > bestScore) {{ bestScore = s; best = c; }}
                        }}
                        if (best && bestScore >= 30) {{ el = best; }}
                    }}
                }}
                if (el) {{
                    el.scrollIntoView({{ behavior: 'instant', block: 'center' }});
                    el.click();
                    return true;
                }}
                return false;
            }})()
            "#
        );

        let eval_res = page.evaluate(script).await?;
        let clicked = eval_res
            .value()
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if !clicked {
            return Err(anyhow!("Failed to click element with node_id '{}': element not found", node_id));
        }
        Ok(())
    }

    async fn fill(&self, node_id: &str, text: &str) -> Result<()> {
        let page = self.active_page()?;
        let escaped_id = serde_json::to_string(node_id)?;
        let escaped_text = serde_json::to_string(text)?;
        let script = format!(
            r#"
            (() => {{
                const id = {escaped_id};
                const text = {escaped_text};
                let el = document.querySelector('[data-munin-id="' + id + '"]') || document.getElementById(id);
                if (!el) {{
                    // 防漂移回退：按注册表指纹重定位
                    const fp = (window.__munin_registry || {{}})[id];
                    if (fp) {{
                        const cand = Array.from(document.querySelectorAll(fp.tag + ', input, textarea, select'));
                        let best = null, bestScore = 0;
                        for (const c of cand) {{
                            let s = 0;
                            if (fp.id_attr && c.id === fp.id_attr) s += 100;
                            if (fp.name && c.getAttribute('name') === fp.name) s += 50;
                            if (fp.aria && c.getAttribute('aria-label') === fp.aria) s += 40;
                            if (fp.placeholder && c.getAttribute('placeholder') === fp.placeholder) s += 40;
                            if (s > bestScore) {{ bestScore = s; best = c; }}
                        }}
                        if (best && bestScore >= 40) {{ el = best; }}
                    }}
                }}
                if (!el) return false;
                el.focus();
                // Vue3/React 受控组件：须走原生 setter 触发响应式依赖追踪
                const proto = el instanceof HTMLTextAreaElement
                    ? HTMLTextAreaElement.prototype
                    : el instanceof HTMLSelectElement
                        ? HTMLSelectElement.prototype
                        : HTMLInputElement.prototype;
                const nativeSetter = Object.getOwnPropertyDescriptor(proto, 'value');
                if (nativeSetter && nativeSetter.set) {{
                    nativeSetter.set.call(el, text);
                }} else {{
                    el.value = text;
                }}
                // 完整事件链：keydown → input(冒泡) → change → blur，驱动 v-model 与 FormRule 校验
                el.dispatchEvent(new KeyboardEvent('keydown', {{ bubbles: true }}));
                el.dispatchEvent(new Event('input', {{ bubbles: true }}));
                el.dispatchEvent(new Event('change', {{ bubbles: true }}));
                el.blur();
                return true;
            }})()
            "#
        );

        let eval_res = page.evaluate(script).await?;
        let filled = eval_res
            .value()
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if !filled {
            return Err(anyhow!("Failed to fill element with node_id '{}': element not found", node_id));
        }
        Ok(())
    }

    async fn evaluate_js(&self, script: &str) -> Result<serde_json::Value> {
        let page = self.active_page()?;
        let trimmed = script.trim();
        let wrapped = if trimmed.starts_with("(() =>") || trimmed.starts_with("(function") {
            trimmed.to_string()
        } else {
            // 检测多行/语句块：含分号、换行、const/let/var 声明 → 直接包裹为自执行闭包，不强制 return
            let is_statement = trimmed.contains(';')
                || trimmed.contains('\n')
                || trimmed.starts_with("const ")
                || trimmed.starts_with("let ")
                || trimmed.starts_with("var ")
                || trimmed.starts_with("if ")
                || trimmed.starts_with("for ")
                || trimmed.starts_with("while ");
            if is_statement {
                format!("(() => {{ {} }})()", trimmed)
            } else {
                format!("(() => {{ try {{ return ({}); }} catch(e) {{ return null; }} }})()", trimmed)
            }
        };
        let eval_fut = page.evaluate(wrapped);
        let eval_res = tokio::time::timeout(std::time::Duration::from_secs(3), eval_fut)
            .await
            .map_err(|_| anyhow!("CDP evaluate timed out waiting for execution context"))??;
        Ok(eval_res.value().cloned().unwrap_or(serde_json::Value::Null))
    }
    async fn take_screenshot(&self) -> Result<Vec<u8>> {
        let page = self.active_page()?;
        let params = ScreenshotParams::builder().build();
        let bytes = page.screenshot(params).await?;
        Ok(bytes)
    }

    /// 等待 DOM 稳定：无节点增删、无 CSS transitions 运行中
    async fn wait_for_stable(&self) -> Result<()> {
        let page = self.active_page()?;
        let script = r#"
            (() => {
                return new Promise((resolve) => {
                    let timer = null;
                    const check = () => {
                        // 检测 CSS transitions 是否运行中
                        const running = document.getAnimations().some(a => a.playState === 'running');
                        if (!running) {
                            clearTimeout(timer);
                            resolve(true);
                        } else {
                            timer = setTimeout(check, 100);
                        }
                    };
                    // 先等 50ms 让动画启动
                    setTimeout(check, 50);
                    // 兜底 3s 强制返回
                    setTimeout(() => resolve(true), 3000);
                });
            })()
        "#;
        let eval_fut = page.evaluate(script);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(4), eval_fut).await;
        Ok(())
    }

    /// AntD DatePicker：打开面板 → 点击对应日期单元格
    async fn pick_date(&self, node_id: &str, date: &str) -> Result<()> {
        let page = self.active_page()?;
        let escaped_id = serde_json::to_string(node_id)?;
        let escaped_date = serde_json::to_string(date)?;
        let script = format!(
            r#"
            (() => {{
                const id = {escaped_id};
                const date = {escaped_date};
                const el = document.querySelector('[data-munin-id="' + id + '"]') || document.getElementById(id);
                if (!el) return false;
                // 打开 DatePicker 面板
                el.dispatchEvent(new MouseEvent('mousedown', {{ bubbles: true }}));
                el.click();
                // 等待面板渲染后点击目标日期
                setTimeout(() => {{
                    const cell = document.querySelector('.ant-picker-cell[title="' + date + '"]')
                        || document.querySelector('.ant-picker-cell-in-view[title="' + date + '"]');
                    if (cell) {{
                        cell.dispatchEvent(new MouseEvent('mousedown', {{ bubbles: true }}));
                        cell.click();
                    }}
                }}, 200);
                return true;
            }})()
            "#
        );
        let eval_res = page.evaluate(script).await?;
        let opened = eval_res.value().and_then(|v| v.as_bool()).unwrap_or(false);
        if !opened {
            return Err(anyhow::anyhow!("Failed to open DatePicker with node_id '{}'", node_id));
        }
        // 等待面板动画稳定
        self.wait_for_stable().await?;
        Ok(())
    }

    /// 下拉选择统一原语：原生 <select> 直接设值；AntD/Element 浮层走 打开→等挂载→匹配→点击
    async fn select_dropdown(&self, node_id: &str, label: &str) -> Result<()> {
        let page = self.active_page()?;
        let escaped_id = serde_json::to_string(node_id)?;
        let escaped_label = serde_json::to_string(label)?;
        let script = format!(
            r#"
            (() => {{
                const id = {escaped_id};
                const label = {escaped_label};
                const squash = (s) => (s || '').replace(/\s+/g, '');
                const labelSq = squash(label);
                const el = document.querySelector('[data-munin-id="' + id + '"]') || document.getElementById(id);
                if (!el) return 'not-found';
                // 原生 <select>：直接匹配 option 设值 + change 事件，无浮层
                if (el.tagName === 'SELECT') {{
                    const opts = Array.from(el.options);
                    const hit = opts.find(o => o.text.trim() === label || o.value === label)
                        || opts.find(o => squash(o.text) === labelSq)
                        || opts.find(o => squash(o.text).includes(labelSq));
                    if (!hit) return 'option-not-found';
                    el.value = hit.value;
                    el.dispatchEvent(new Event('input', {{ bubbles: true }}));
                    el.dispatchEvent(new Event('change', {{ bubbles: true }}));
                    return 'native-ok';
                }}
                // 复合组件触发器升级：内层 input/combobox 须冒泡到 .ant-select-selector（AntD 4.x 浮层挂载依赖其 mousedown）
                const trigger = el.closest('.ant-select-selector')
                    || el.closest('.ant-select, .el-select')
                    || el;
                trigger.dispatchEvent(new MouseEvent('mousedown', {{ bubbles: true }}));
                trigger.click();
                return 'opened';
            }})()
            "#
        );
        let eval_res = page.evaluate(script).await?;
        let status = eval_res.value().and_then(|v| v.as_str()).unwrap_or("").to_string();
        match status.as_str() {
            "not-found" => return Err(anyhow::anyhow!("select_option: element '{node_id}' not found")),
            "option-not-found" => return Err(anyhow::anyhow!("select_option: no option matching '{label}' in native select '{node_id}'")),
            "native-ok" => return Ok(()),
            _ => {}
        }

        // 等待浮层挂载（AntD/Element 下拉 Portal 挂在 body 底部，渲染有延迟）
        let dropdown_sel = ".ant-select-dropdown:not(.ant-select-dropdown-hidden) .ant-select-item-option, .el-select-dropdown:not([style*='display: none']) .el-select-dropdown__item";
        if self.wait_for(dropdown_sel, "attached", 3000).await.is_err() {
            return Err(anyhow::anyhow!("select_option: dropdown panel did not mount within 3s for '{node_id}'"));
        }

        // 在浮层内匹配 label（精确 → 归一化 → 包含）并点击
        let click_script = format!(
            r#"
            (() => {{
                const label = {escaped_label};
                const squash = (s) => (s || '').replace(/\s+/g, '');
                const labelSq = squash(label);
                const opts = Array.from(document.querySelectorAll(
                    '.ant-select-dropdown:not(.ant-select-dropdown-hidden) .ant-select-item-option, ' +
                    '.el-select-dropdown:not([style*="display: none"]) .el-select-dropdown__item'
                )).filter(o => o.offsetParent !== null);
                const hit = opts.find(o => o.textContent.trim() === label || o.getAttribute('title') === label)
                    || opts.find(o => squash(o.textContent) === labelSq)
                    || opts.find(o => labelSq && squash(o.textContent).includes(labelSq));
                if (!hit) return false;
                hit.dispatchEvent(new MouseEvent('mousedown', {{ bubbles: true }}));
                hit.click();
                return true;
            }})()
            "#
        );
        let res = page.evaluate(click_script).await?;
        let clicked = res.value().and_then(|v| v.as_bool()).unwrap_or(false);
        if !clicked {
            return Err(anyhow::anyhow!("select_option: no visible option matching '{label}' in dropdown"));
        }
        self.wait_for_stable().await?;
        Ok(())
    }
}
