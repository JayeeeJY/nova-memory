'use strict';
// Describe only the saved selection and compiled adapter; never probe a model or read credentials.
(()=>{
 const panel=$('provider-capabilities'),host=$('provider-summary'),status=$('provider-status');let epoch=0;
 const active=()=>!$('workspace').hidden&&!$('native-settings-panel').hidden&&panel.open;
 function clear(){epoch++;host.replaceChildren();status.textContent='';}
 async function refresh(){if(!active())return;const e=++epoch;host.replaceChildren();status.textContent='正在读取当前配置…';try{const r=await api('/model-capabilities');if(e!==epoch||!active())return;
  const blockers={backup_quarantined:'此备份副本保持隔离，整理不会启动。',model_not_configured:'尚未选择整理模型。材料会先加密保存。',runtime_not_enabled:'已选择模型，整理进程尚未启用。',configuration_not_admissible:'当前模型设置与运行协议不匹配，需要调整后才能整理。'};
  status.textContent=blockers[r.blocker]||'配置通过静态检查。服务是否可用、模型处理结果是否准确仍需实际验证。';
  const location={unconfigured:'尚未配置',loopback_service_configured:'本机环回服务',remote_service_configured:'你指定的远程服务'}[r.processing.configured_location]||'未知';
  host.append(text('p','当前选择：'+(r.selected_model||'未选择模型')+' · '+location),text('p','实际处理位置尚未核验。这里不会启动模型调用，也不会读取服务凭据。'));
  if(r.adapter){const a=r.adapter,details=document.createElement('details');details.append(text('summary','协议与运行限制'),text('p',a.streaming?'当前请求 JSON Schema 与流式返回；服务端是否支持仍待核验。':'当前请求 JSON 对象，等待完整返回；服务端是否支持仍待核验。'),text('p','每次请求最多 '+a.limits.request_max_output_tokens+' 个输出 tokens，传输截止 '+a.limits.transport_timeout_seconds+' 秒，本库最多 '+a.limits.maximum_calls+' 次派发。'),text('p','截止会结束本机传输；供应商是否停止计算未知。'));host.append(details);}
  host.append(text('p',r.adapter?.usage.normal_worker_persists_usage?'普通整理会按尝试加密保存模型报告的 token 用量；可在 AI 整理的运行诊断中查看。未报告的用量和供应商费用保持未知；未知不表示免费。':'当前整理运行器未启用，模型用量和费用不能从配置推算；未知不表示免费。'),text('p','失败时保留原材料，不自动重试或切换模型、服务地址。修改处理位置仍需你明确配置。'));
 }catch{if(e===epoch&&active()){host.replaceChildren();status.textContent='暂时无法读取配置说明。请刷新；本次未启动模型。';}}}
 panel.addEventListener('toggle',()=>{if(panel.open)refresh();else clear();});$('provider-refresh').addEventListener('click',refresh);
 new MutationObserver(()=>{if(active())refresh();else clear();}).observe($('native-settings-panel'),{attributes:true,attributeFilter:['hidden']});
 new MutationObserver(()=>{if($('workspace').hidden)clear();}).observe($('workspace'),{attributes:true,attributeFilter:['hidden']});
 window.addEventListener('nova:intake-mutation',e=>{if(e.detail?.path==='/configuration'&&active())refresh();});
 document.addEventListener('visibilitychange',()=>{if(document.hidden)clear();else refresh();});window.addEventListener('pagehide',clear);
})();
