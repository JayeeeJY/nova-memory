'use strict';
// N5 Owner-only metadata reports. All labels and imported observations are data,
// never HTML, instructions, or evidence of actual model or Host acceptance.
(()=>{
 const panel=document.getElementById('memory-report');if(!panel)return;
 const el=id=>document.getElementById(id),make=(tag,value,className)=>{const node=document.createElement(tag);if(value!==undefined&&value!==null)node.textContent=String(value);if(className)node.className=className;return node;};
 const state={data:null,epoch:0,lifecycle:0,busy:false,signature:'',importing:false,suspended:false};
 let refreshTimer=0,mutationTimer=0;const downloadUrls=new Set();
 const armLabels={no_memory:'无记忆',raw_retrieval:'原文检索',compiled:'结构化记忆',relations:'结构与关系'};
 const relationLabels={parallel_evidence:'不同来源的平行依据',conditional_variant:'条件差异',temporal_transition:'时间变化',potential_conflict:'差异待核对'};
 const categoryLabels={facts:'事实与来源',cross_material:'跨材料',time:'时间',updates:'更新与例外',unknown_conflict:'未知与冲突',privacy:'隐私边界'};
 const metadataLabels={user:'用户材料',assistant:'AI 材料',system:'系统材料',external:'外部材料',agent:'Agent 材料',mixed:'混合角色',unknown:'尚未识别',f1_ai_digest:'AI 摘要',f2_visible_transcript:'可见记录',f3_raw_audio:'原始音频',user_statement:'用户自述',extracted:'AI 提取',inferred:'AI 推断',external_claim:'外部材料陈述',agent_experience:'Agent 经历记录',private:'普通私密',shareable:'可分享',received:'已接收',processing:'整理中',failed:'整理失败',ready:'整理完成'};
 const date=value=>{const parsed=new Date(value);return value&&!Number.isNaN(parsed.valueOf())?parsed.toLocaleString('zh-CN',{hour12:false}):'尚未记录';};
 function active(){return !state.suspended&&!document.hidden&&!el('workspace').hidden&&!el('view-space').hidden&&!panel.hidden;}
 function validCount(value){return Number.isSafeInteger(value)&&value>=0;}
 function count(value){return validCount(value)?String(value):'未记录';}
 function textValue(value){return typeof value==='string'&&value?value:'尚未记录';}
 function revokeDownloads(){for(const url of downloadUrls)URL.revokeObjectURL(url);downloadUrls.clear();}
 function clear(message='报告已清除，打开后重新读取。',mode='idle'){
  state.data=null;state.signature='';state.busy=false;el('report-content').replaceChildren();el('report-download').disabled=true;el('report-status').textContent=message;panel.dataset.state=mode;delete panel.dataset.snapshot;delete panel.dataset.evaluationCount;el('report-import').open=false;el('report-import-file').value='';revokeDownloads();
 }
 function controls(){el('report-refresh').disabled=state.busy;el('report-import-submit').disabled=state.importing;el('report-download').disabled=!state.data;}
 function invalidate(message='材料、版本或连接已变化。旧报告已清除，正在重新读取。'){
  state.epoch++;clearTimeout(refreshTimer);clearTimeout(mutationTimer);clear(message,'loading');controls();
  if(active())mutationTimer=setTimeout(()=>load(),150);
 }
 function check(data){
  const construction=data?.construction,c=construction?.construction,m=construction?.metadata,u=construction?.usage;
  if(!construction||construction.schema_version!=='nova.construction-report.v0.1'||!construction.scope||typeof construction.content_snapshot_id!=='string'||construction.completeness?.complete!==true||construction.completeness?.count_scope!=='eligible_current_library'||!c||!m||!u||!Array.isArray(data.task_evaluations?.items)||data.task_evaluations.items.length>64)throw new Error('invalid report');
  for(const key of ['sources','uncompiled_sources','extraction_records','exact_memories','topics','source_links','current_revisions','historical_revisions','wiki_fresh','wiki_pending','wiki_blocked','wiki_stale','wiki_cold'])if(!validCount(c[key]))throw new Error('invalid construction count');
  for(const key of ['configured_connections','active_connections','revoked_connections','expired_connections','logical_calls','delivered_items'])if(!validCount(u[key]))throw new Error('invalid usage count');
  for(const stored of data.task_evaluations.items){const r=stored?.report;if(typeof stored?.evaluation_id!=='string'||typeof stored.asset_snapshot_id!=='string'||typeof stored.stale!=='boolean'||r?.schema_version!=='nova.task-evaluation-report.v0.1'||r.evaluation_scope!=='synthetic_fixed_tasks'||!['fixture_contract','owner_reported_observations'].includes(r.run_kind)||r.quality_status!=='independent_quality_uncalibrated'||r.level_status!=='uncalibrated'||!Array.isArray(r.metrics)||r.metrics.length!==4||new Set(r.metrics.map(a=>a.arm)).size!==4||r.metrics.some(a=>!armLabels[a.arm]))throw new Error('invalid evaluation report');}
  return data;
 }
 async function load(){
  if(!active()||state.busy)return;clearTimeout(refreshTimer);const epoch=++state.epoch;state.busy=true;controls();if(!state.data){panel.dataset.state='loading';el('report-status').textContent='正在校验当前范围、版本与评测记录…';}
  try{
   const data=check(await api('/report'));if(epoch!==state.epoch||!active())return;state.data=data;
   const c=data.construction,signature=JSON.stringify([c.content_snapshot_id,c.construction,c.metadata,c.usage,data.task_evaluations.items]);
   if(signature!==state.signature){state.signature=signature;render(data);}else{const asof=el('report-asof');if(asof)asof.textContent=date(c.generated_at);}
   panel.dataset.state='ready';panel.dataset.snapshot=c.content_snapshot_id;panel.dataset.evaluationCount=String(data.task_evaluations.items.length);el('report-status').textContent='已读取当前可计数范围。建设情况与固定任务评测分别呈现；未测量项保留为未知。';
  }catch{if(epoch!==state.epoch||!active())return;clear('报告暂时无法校验或超过当前统计上限。旧报告已清除，请检查服务后重试。','error');}
  finally{if(epoch===state.epoch){state.busy=false;controls();if(active())refreshTimer=setTimeout(load,10000);}}
 }
 function section(parent,title,note){const node=make('section',undefined,'report-section');node.append(make('h3',title));if(note)node.append(make('p',note));parent.append(node);return node;}
 function field(dl,label,value,id){const term=make('dt',label),definition=make('dd',value);if(id)definition.id=id;dl.append(term,definition);}
 function listCounts(parent,rows){const grid=make('div',undefined,'report-counts');for(const [label,value]of rows){const row=make('div');row.append(make('span',label),make('strong',value));grid.append(row);}parent.append(grid);}
 function details(parent,title){const node=make('details',undefined,'report-details');node.append(make('summary',title));parent.append(node);return node;}
 function mapRows(map,labels=metadataLabels){return Object.entries(map||{}).map(([key,value])=>[labels[key]||key,count(value)]);}
 function table(parent,caption,headers,rows){const wrapper=make('div',undefined,'report-table-wrap'),table=make('table',undefined,'report-table');table.append(make('caption',caption));const thead=make('thead'),head=make('tr');for(const title of headers){const th=make('th',title);th.scope='col';head.append(th);}thead.append(head);table.append(thead);const body=make('tbody');for(const row of rows){const tr=make('tr');row.forEach((value,index)=>{const cell=make(index?'td':'th');if(!index)cell.scope='row';if(value instanceof Node)cell.append(value);else cell.textContent=String(value);tr.append(cell);});body.append(tr);}table.append(body);wrapper.append(table);parent.append(wrapper);}
 function render(data){
  const root=el('report-content');root.replaceChildren();renderConstruction(root,data.construction);renderEvaluations(root,data.task_evaluations.items);
 }
 function renderConstruction(root,report){
  const c=report.construction,m=report.metadata,u=report.usage;
  const overview=section(root,'记忆库建设','统计当前可读、有效且未撤回的普通私密与可分享材料与记录。语义陈述仍须独立验证；这份全范围统计与记忆球的当前页投影分别计算。');
  const meta=make('dl',undefined,'report-meta');field(meta,'统计时点',date(report.generated_at),'report-asof');field(meta,'本库范围',textValue(report.scope.owner_id)+' · '+textValue(report.scope.space));field(meta,'材料与规则快照',report.content_snapshot_id);field(meta,'统计完整性','当前可计数范围完整；不含敏感或封存材料。');overview.append(meta);
  const stats=make('div',undefined,'report-stats');for(const [key,label,note]of [['sources','原始材料','当前可计数的来源'],['extraction_records','提取记录','保留独立版本与证据性质'],['exact_memories','精确归并记忆','重复陈述不会增加对象数'],['topics','结构主题','主体与属性分区'],['source_links','来源关联','归并对象 × 来源的唯一配对'],['historical_revisions','保留的历史版本','历史数量不等于独立知识']]){const card=make('article',undefined,'report-stat');card.append(make('span',label),make('strong',count(c[key])),make('small',note));stats.append(card);}overview.append(stats);
  const current=details(overview,'查看整理、版本与关系口径');listCounts(current,[['待完成编译的材料',count(c.uncompiled_sources)],['当前无提取记录的材料',count(c.sources_without_current_records)],['当前版本记录',count(c.current_revisions)],['已有适用条件的记录',count(m.conditioned_records)],['有明确适用时间的记录',count(m.known_valid_time_records)],['适用时间未知的记录',count(m.unknown_valid_time_records)],['有材料所述时间的记录',count(m.known_occurrence_records)],['材料所述时间未知的记录',count(m.unknown_occurrence_records)],['保留用户纠正的记录',count(m.protected_records)],['带派生依据的记录',count(m.records_with_derivation)],['带结构模块的记录',count(m.records_with_modules)]]);current.append(make('p','成功整理后没有提取长期记忆的材料，不算作“待完成编译”。精确归并只合并完全相同的结构陈述，不据此判断知识质量或来源独立性。'));
  listCounts(current,[['当前主题视图',count(c.wiki_fresh)],['等待整理的主题',count(c.wiki_pending)],['等待处理的主题',count(c.wiki_blocked)],['依据已变化的主题',count(c.wiki_stale)],['缓存已清理的主题',count(c.wiki_cold)],...mapRows(c.rule_associations,relationLabels)]);current.append(make('p','关系只统计当前有效视图、可读端点之间的结构关联。关系条数不代表理解深度、因果成立或已验证的冲突。'));
  const metadata=details(overview,'查看材料与陈述性质');for(const [key,label]of [['source_roles','来源角色'],['capture_fidelity','采集保真度'],['semantic_origins','陈述形成方式'],['privacy_levels','当前可计数隐私范围'],['job_states','整理任务状态']]){metadata.append(make('h4',label));const rows=mapRows(m[key]);if(rows.length)listCounts(metadata,rows);else metadata.append(make('p','当前范围尚未记录。','report-no-data'));}
  const usage=section(root,'使用与可控性','以下为本机服务端记录；服务端交付回执不等于实际 AI Host 已接入、理解或完成任务。');listCounts(usage,[['已配置的连接',count(u.configured_connections)],['当前有效连接',count(u.active_connections)],['已撤销连接',count(u.revoked_connections)],['已到期连接',count(u.expired_connections)],['额度已用尽的连接',count(u.exhausted_connections)],['逻辑操作次数',count(u.logical_calls)],['已交付的记忆条次',count(u.delivered_items)]]);const usageRows=mapRows(u.operations,{read:'读取',capture:'采集',save:'保存'});if(usageRows.length){const d=details(usage,'查看操作类型');listCounts(d,usageRows);}usage.append(make('small','同一次幂等重放不累计为新的逻辑操作；交付条次可包含多次读取同一条记忆，不是独立知识量。'));
  const unknown=section(root,'还需要实测的部分','以下指标没有可用的独立测量，保持未知。没有把未知显示成 0，也没有从记忆体量生成能力分数。');const grid=make('div',undefined,'report-unknowns');for(const label of ['目标覆盖率','语义正确性','实际模型 tokens','实际模型费用','个人 Agent 任务收益','能力等级校准']){const item=make('div');item.append(make('span',label),make('strong',label.includes('等级')?'尚未校准':'未测量'));grid.append(item);}unknown.append(grid);unknown.append(make('small','目标覆盖率需要先定义“应该记住什么”；固定合成任务的运行记录也不能直接证明当前个人库的收益。'));
 }
 function observed(summary,unit,aggregation){
  if(!summary||typeof summary.value!=='number'||!Number.isFinite(summary.value))return '未测量';
  const node=make('span',summary.value.toLocaleString('zh-CN',{maximumFractionDigits:unit==='$'?6:2})+(unit?' '+unit:''));node.append(make('small',aggregation+' · '+count(summary.observed)+' / '+count(summary.total)+' 条已记录'));return node;
 }
 function renderEvaluations(root,items){
  const tasks=section(root,'Agent 任务收益','使用固定任务、相同配置与四组对照进行比较。评测记录来自独立导入文件；不会自动运行模型或发送个人材料。');
  if(!items.length){const cold=make('div',undefined,'report-coldstart');cold.append(make('h4','尚未评测'),make('p','建设情况已经可以查看。任务收益需要运行与复核后再记录，目前没有成功率、增益或等级。'));table(cold,'计划采用的四组对照',['对照组','当前状态'],Object.values(armLabels).map(label=>[label,'未评测']));cold.append(make('p','独立 Gold、保留集与阈值校准尚待完成。真实模型与真实 AI Host 分别验收。'));tasks.append(cold);return;}
  tasks.append(make('p','当前 '+items.length+' 份评测记录。所有固定任务结果均为合成评测范围，不表示当前个人记忆库的效果；未完成独立质量校准。'));
  for(const stored of items){
   const report=stored.report,fixture=report.run_kind==='fixture_contract';const article=make('article',undefined,'report-evaluation');article.dataset.evaluationId=stored.evaluation_id;article.dataset.runKind=report.run_kind;article.append(make('span',fixture?'工程演练，不是模型实测':'用户提供的运行记录，尚未独立核验','report-tag'+(fixture?' warning':'')));
   article.append(make('h4',fixture?'固定任务评分流程演练':'固定合成任务的运行记录'));article.append(make('p','独立语义质量尚未校准；Gold 与阈值仍待独立验证。','report-boundary'));if(stored.stale)article.append(make('p','本库材料或规则已变化。这是历史快照的记录，不能代表当前库。','report-boundary'));
   const dl=make('dl',undefined,'report-meta');for(const [label,value]of [['运行完成',date(report.completed_at)],['本机记录',date(stored.recorded_at)],['模型与版本',textValue(report.model_id)+' · '+textValue(report.model_version)],['Agent / 提示词 / 规则',textValue(report.agent_version)+' / '+textValue(report.prompt_version)+' / '+textValue(report.rules_version)],['任务范围',count(report.task_count)+' 个固定合成任务；含 '+count(report.structure_evolution_tasks)+' 个结构演化任务'],['当前库快照匹配',stored.stale?'已变化，仅保留历史记录':'快照一致；未证明个人任务收益']])field(dl,label,value);article.append(dl);
   const metrics=Object.keys(armLabels).map(arm=>report.metrics.find(item=>item.arm===arm));
   table(article,fixture?'工程断言计数；不作为真实模型任务收益':'固定评分器的报告计数；语义尚未独立复核',['对照组',fixture?'工程断言通过 / 任务数':'评分器通过 / 任务数','来源集合匹配记录','错误 / 过期','越界 / 伪造'],metrics.map(item=>[armLabels[item.arm],count(item.success_count)+' / '+count(item.cases),count(item.source_correct_count),count(item.error_count)+' / '+count(item.outdated_count),count(item.privacy_violation_count)+' / '+count(item.source_forgery_count)]));
   const compare=details(article,'查看与无记忆的成对比较');table(compare,'同一任务的成对计数；不等于能力等级',['对照组','改善','退步','无变化'],(report.pairs||[]).map(pair=>[armLabels[pair.comparison_arm]||pair.comparison_arm,count(pair.improved),count(pair.regressed),count(pair.unchanged)]));compare.append(make('p','仅反映这组固定任务和评分规则。来源集合匹配只核对固定参考 ID，不证明引用在语义上支持答案。工程演练通过不能替代真实模型实测；用户提供的观测也需要独立复核。'));
   const categories=details(article,'查看任务分类与边界记录');for(const arm of metrics){categories.append(make('h4',armLabels[arm.arm]));categories.append(make('p','未知情形按规则处理 '+count(arm.unknown_handled_count)+' 条；这项也是评分器的报告计数。'));const rows=(arm.categories||[]).map(category=>[categoryLabels[category.category]||category.category,count(category.success_count)+' / '+count(category.cases)]);if(rows.length)table(categories,'分类计数',['任务分类','通过 / 任务数'],rows);}listCounts(categories,[['来源伪造记录',count(report.source_forgery_count)],['严重越界记录',count(report.severe_boundary_count)],['工程安全门槛记录',report.safety_gate===true?'满足记录要求':'未满足记录要求']]);categories.append(make('p','安全门槛仅表示此评分记录满足相应断言；真实权限安全与新 AI Host 仍须分别验收。'));
   const performance=details(article,'查看已记录的延迟、tokens 与费用');table(performance,'只显示提供了观测值的记录；缺少费用不估算',['对照组','已观测平均延迟','已观测输入 tokens','已观测输出 tokens','已观测调用费用'],metrics.map(item=>[armLabels[item.arm],observed(item.latency_ms,'ms','平均值'),observed(item.input_tokens,'','部分合计'),observed(item.output_tokens,'','部分合计'),observed(item.cost_usd,'$','部分合计')]));listCounts(performance,[['摄入费用（USD）',report.costs?.ingest_usd===null||report.costs?.ingest_usd===undefined?'尚未记录':String(report.costs.ingest_usd)],['后台维护费用（USD）',report.costs?.maintenance_usd===null||report.costs?.maintenance_usd===undefined?'尚未记录':String(report.costs.maintenance_usd)]]);performance.append(make('p','tokens 与调用费用按已记录样本合计；观测不完整时，这不是完整总费用。摄入与后台维护费用另外记录。'));performance.append(make('p',fixture?'工程演练中的观测均为人工合成字段，不是模型实际耗时、tokens 或账单。':'运行记录的观测由文件提供，未独立核对模型日志或账单。'));
   const provenance=details(article,'查看评测版本与记录说明');const provenanceDl=make('dl',undefined,'report-meta');for(const [label,value]of [['评测标识',stored.evaluation_id],['对应材料快照',stored.asset_snapshot_id],['任务集指纹',report.dataset_hash],['配置指纹',report.configuration_hash],['质量状态','未独立验证与校准'],['等级状态','尚未校准']])field(provenanceDl,label,textValue(value));provenance.append(provenanceDl,make('p',textValue(report.evidence_note),'report-evidence-note'));tasks.append(article);
  }
 }
 function activityChanged(){
  if(active()){load();return;}state.epoch++;state.lifecycle++;state.importing=false;clearTimeout(refreshTimer);clearTimeout(mutationTimer);clear('回到报告时，重新读取当前范围与评测记录。');el('report-import-file').value='';el('report-import-status').textContent='';el('report-import').open=false;controls();
 }
 el('report-open').addEventListener('click',()=>{if(el('workspace').hidden)return;panel.hidden=false;el('report-open').setAttribute('aria-expanded','true');load();el('report-heading').focus({preventScroll:true});panel.scrollIntoView({behavior:'instant',block:'start'});});
 el('report-close').addEventListener('click',()=>{panel.hidden=true;el('report-open').setAttribute('aria-expanded','false');activityChanged();el('report-open').focus({preventScroll:true});});
 el('report-refresh').addEventListener('click',()=>invalidate('正在重新校验当前报告…'));
 el('report-download').addEventListener('click',()=>{if(!active()||!state.data)return;const blob=new Blob([JSON.stringify(state.data,null,2)],{type:'application/json'}),url=URL.createObjectURL(blob),anchor=make('a');downloadUrls.add(url);anchor.href=url;anchor.download='nova-memory-private-report.json';anchor.click();setTimeout(()=>{URL.revokeObjectURL(url);downloadUrls.delete(url);},10000);});
 el('report-import-file').addEventListener('change',()=>{el('report-import-status').textContent='';});
 el('report-import-form').addEventListener('submit',async event=>{
  event.preventDefault();if(!active()||state.importing)return;const file=el('report-import-file').files[0];if(!file)return;
  if(file.size>512*1024){el('report-import-status').textContent='文件超过 512 KiB，请使用离线评测器生成的有界结果文件。';return;}
  const lifecycle=state.lifecycle;state.importing=true;controls();el('report-import-status').textContent='正在校验文件与固定任务结果…';
  try{const source=await file.text();if(lifecycle!==state.lifecycle||!active())return;const artifact=JSON.parse(source);if(!artifact||typeof artifact!=='object'||Array.isArray(artifact))throw new Error('invalid file');await api('/task-evaluations',artifact);if(lifecycle!==state.lifecycle||!active())return;el('report-import-file').value='';el('report-import-status').textContent='已记录。导入只验证评分记录，不代表真实模型、独立语义质量或 AI Host 已验收。';await load();}
  catch{if(lifecycle===state.lifecycle&&active())el('report-import-status').textContent='这份文件未能导入。请使用受支持的离线评测结果，确认固定任务、四组对照与记录完整；也可稍后重试。';}
  finally{if(lifecycle===state.lifecycle){state.importing=false;controls();}}
 });
 window.addEventListener('nova:viewchange',activityChanged);document.addEventListener('visibilitychange',activityChanged);window.addEventListener('nova:intake-mutation',()=>{el('report-import').open=false;invalidate();});window.addEventListener('pagehide',()=>{state.suspended=true;activityChanged();});window.addEventListener('pageshow',()=>{state.suspended=false;activityChanged();});el('lock').addEventListener('click',()=>{panel.hidden=true;activityChanged();});new MutationObserver(activityChanged).observe(el('workspace'),{attributes:true,attributeFilter:['hidden']});
 window.NovaMemoryReport=Object.freeze({invalidate});
})();
