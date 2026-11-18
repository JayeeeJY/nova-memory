'use strict';
(()=>{
 const panel=$('diagnostics-panel'),status=$('diagnostics-status'),list=$('diagnostics-layers');let epoch=0,busy=false;
 const names={compiler_queue:'整理队列',capture_grants:'采集权限',capture_receipts:'采集回执',connections:'记忆连接',disclosure:'读取与披露'};
 const labels={waiting:'等待整理',processing_lease_observed:'正在处理（租约有效）',expired_lease:'处理租约已过期',ready:'结果已入库',failed:'处理失败',model_unavailable:'模型暂不可用',invalid_output:'输出校验失败',budget_exceeded:'预算已达上限',worker_interrupted:'工作进程中断',configuration_changed:'配置已变化',other_failure:'其他失败',active:'有效',revoked:'已撤销',expired:'已到期',exhausted:'额度已用尽',source_acked:'已接收原文',truncated:'内容有截断',redacted:'内容有脱敏',partial_history:'仅部分历史',read_receipts:'已记录读取',empty_read:'其中未返回记忆',capture_receipts:'已记录保存',other_receipts:'其他操作回执'};
 function clear(){epoch++;list.replaceChildren();status.textContent='';}
 $('diagnostics-refresh').addEventListener('click',async()=>{
  if(busy)return;busy=true;const e=epoch;$('diagnostics-refresh').disabled=true;status.textContent='正在检查本机状态…';list.replaceChildren();
  try{
   const r=await api('/diagnostics');if(e!==epoch||$('workspace').hidden)return;
   const location={unconfigured:'整理模型尚未配置',local:'已选择本机模型',remote:'已选择远程模型'}[r.store.model_location]||'模型配置待检查';
   status.textContent=location+'。'+(r.runtime.compiler_configured?'整理服务已配置；实际运行情况见队列记录。':'整理服务未启用，已保存材料仍保留。')+' 本次检查未调用模型。';
   const disk=document.createElement('article');disk.className='record';disk.append(text('h3','本机存储'));const bytes=r.disk.available_bytes;disk.append(text('p',typeof bytes==='number'?'当前磁盘可用约 '+(bytes/(1024**3)).toFixed(2)+' GiB。此数值是检查时的可用空间，不是已预留容量。':'未能读取可用空间，请检查本机存储。'));
   if(['low','critical'].includes(r.disk.state))disk.append(text('p','可用空间较低。请先腾出磁盘空间，并核对未收到回执的材料；不要删除 Vault 或密钥。'));
   list.append(disk);
   const backup=document.createElement('article');backup.className='record';backup.append(text('h3','备份与恢复'));
   backup.append(text('p',r.backup.last_generated_at?'最近生成备份：'+new Date(r.backup.last_generated_at).toLocaleString('zh-CN')+'。请确认已经另存，并单独保管 Vault 密钥；此记录不证明文件已保存到其他设备。':'尚无备份生成记录。可以在“设置与数据”中下载加密备份。'));
   if(r.backup.quarantined_copy)backup.append(text('p','当前是隔离的备份副本，仅供本人查验。后续删除与撤权尚未核对，模型任务和 AI 连接保持关闭。'));
   list.append(backup);
   const meter=document.createElement('article');meter.className='record';meter.id='model-usage-observations';meter.append(text('h3','最近模型用量'));
   meter.append(text('p','仅记录本机整理服务观测到的用量，不代表费用、入库成功或模型质量。不同模型不合并计算；历史未记录的调用不会补猜。'));
   const observations=r.model_usage?.items||[];
   if(!observations.length)meter.append(text('p','尚无模型用量记录。运行诊断不会调用模型。'));
   const outcomes={response_received:'响应已接收',response_rejected:'响应未通过校验',transport_failed:'传输未完成',dispatch_not_started:'请求未发出',worker_interrupted:'工作进程中断'};
   for(const item of observations.slice(0,5)){
    const row=document.createElement('div');row.className='record';
    row.append(text('h4',item.intent.model_id+' · 第 '+item.intent.attempt+' 次尝试'));
    const observation=item.observed?.observation;
    row.append(text('p',observation?(outcomes[observation.outcome]||'结果未知'):'已批准发起，结果未知。可能在发出请求前或等待响应时中断。'));
    const usage=observation?.usage;
    row.append(text('p',usage?'模型报告：输入 '+usage.input_tokens+' · 输出 '+usage.output_tokens+' · 合计 '+usage.total_tokens+' token。输入中缓存命中：'+(usage.cached_input_tokens===null?'未知':usage.cached_input_tokens)+'。':'模型用量未知，不能按 0 计算。'));
    row.append(text('small',new Date(item.intent.authorized_at).toLocaleString('zh-CN')+' · 金额未测量'));
    meter.append(row);
   }
   if(observations.length>5||r.model_usage?.next_before)meter.append(text('p','此处展示最近 5 次尝试，不是全部历史或账单。'));
   list.append(meter);
   for(const [name,layer] of Object.entries(r.store.layers)){
    const row=document.createElement('article');row.className='record';row.append(text('h3',names[name]||'运行记录'));
    const counts=Object.entries(layer.counts).map(([k,v])=>(labels[k]||'其他记录')+' '+v).join(' · ');row.append(text('p',counts||'本轮检查范围内没有记录。'));
    if(layer.last_success_at)row.append(text('small','最近成功记录：'+new Date(layer.last_success_at).toLocaleString('zh-CN')));
    if(layer.oldest_pending_at)row.append(text('p','本轮最早等待：'+new Date(layer.oldest_pending_at).toLocaleString('zh-CN')));
    if(!layer.complete)row.append(text('p','仅检查最近部分记录；这些数量不是全库总数。'));
    if(layer.omitted_unreadable)row.append(text('p','已失效记录未读取：'+layer.omitted_unreadable));list.append(row);
   }
   list.append(text('p','此检查不验证模型质量、客户端 Hook 安装、端侧离线队列或备份恢复。已接收原文不等于整理完成，连接有效不等于客户端已经使用。'));
  }catch{if(e===epoch){status.textContent='本轮诊断未能完成，当前状态未知。可以稍后重试；此次未执行修复。';list.replaceChildren();}}
  finally{busy=false;$('diagnostics-refresh').disabled=false;}
 });
 new MutationObserver(()=>{if($('workspace').hidden)clear();}).observe($('workspace'),{attributes:true,attributeFilter:['hidden']});window.addEventListener('pagehide',clear);
})();
