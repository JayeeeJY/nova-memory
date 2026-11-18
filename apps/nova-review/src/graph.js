'use strict';
// N4: a bounded Owner-only projection. Screen positions and the faint sphere grid
// are presentation geometry; only server-provided edges represent relationships.
(()=>{
 const panel=document.getElementById('memory-graph');if(!panel)return;
 const byId=id=>document.getElementById(id),canvas=byId('graph-canvas'),ctx=canvas.getContext('2d');
 const kindLabels={source:'来源',memory:'精确归并记忆',topic:'主题视图'};
 const edgeLabels={in_topic:'属于主题',supported_by:'来源支持',parallel_evidence:'不同来源的平行依据',conditional_variant:'条件差异',temporal_transition:'时间变化',potential_conflict:'差异待核对'};
 const privacyLabels={shareable:'可分享',private:'普通私密'};
 const originLabels={user_statement:'用户自述',extracted:'AI 提取',inferred:'AI 推断',external_claim:'外部材料的陈述',agent_experience:'Agent 经历记录'};
 const modalityLabels={asserted:'陈述',planned:'计划',hypothetical:'假设',unknown:'尚不确定'};
 const noticeLabels={wiki_cold_associations_omitted:"部分主题缓存已清理，相关结构关联暂不显示；记忆与来源仍保留。",partition_over_64_records_omitted_no_privacy_assumptions:'部分主题超过 64 条提取记录，暂未展开；没有推测被省略记录的内容或隐私范围。',wiki_pending_or_blocked_associations_omitted:'部分主题正在整理或等待处理，旧关联暂不显示。',wiki_not_current_associations_omitted:'部分主题依据已变化，旧关联暂不显示。',relationship_budget_partial:'实际关系较多，本页尚未全部显示。',projection_budget_partition_omitted:'部分主题超出本页展示上限，暂未展开。',uncompiled_sources_remain_in_intake_not_graph:'尚未整理的材料仍保存在最近接收中，暂不出现在图里。'};
 const commonNotices=new Set(['ordinary_current_only_no_sensitive_or_sealed','page_counts_not_global_inventory','structural_associations_not_verified_semantic_truth']);
 const colors={source:'#718fa5',memory:'#b7ead8',topic:'#c5aff5'};
 const state={data:null,epoch:0,cursor:null,page:1,busy:false,next:null,topic:'',search:'',tab:'nodes',selected:null,baseline:null,mutationPending:false,paused:true,listOnly:false,angle:0,frame:0,points:[],lines:[],lastFrame:0};
 let refreshTimer=0,mutationTimer=0,sourceEpoch=0;
 const reduced=window.matchMedia('(prefers-reduced-motion: reduce)');
 function active(){return !document.hidden&&!byId('workspace').hidden&&!byId('view-space').hidden;}
 function make(tag,value,className){const el=document.createElement(tag);if(value!==undefined&&value!==null)el.textContent=String(value);if(className)el.className=className;return el;}
 function short(id){return String(id||'').length>24?String(id).slice(0,13)+'…'+String(id).slice(-7):String(id||'');}
 function jsonText(v){if(v===null||v===undefined)return '未记录';return typeof v==='string'?v:JSON.stringify(v);}
 function displayLabel(node){if(node.kind==='memory'&&node.summary){try{const value=JSON.parse(node.summary);if(typeof value==='string')return value;}catch{}return node.summary;}if(node.kind==='source')return ({'web-console':'工作台记录','mcp-codex':'Codex 对话','mcp-claude':'Claude 对话'})[node.label]||node.label;return node.label.replace(/^(user|self) · /,'关于我 · ');}
 function statementNature(node){return (node.polarity==='negative'?'否定陈述':node.polarity==='positive'?'肯定陈述':'陈述方向未记录')+' · '+(modalityLabels[node.modality]||'陈述性质未记录');}
 function edgeExplanation(edge){return ({in_topic:'同一主体与属性的结构分区，用于整理；它不代表可信项目归属，也不授予读取权限。',supported_by:'这条当前有效的记忆引用了该来源。能回到原文，不等于语义事实已经核实。',parallel_evidence:'这些记录有平行依据；不同来源、隐私或时间边界仍分别保留。',conditional_variant:'适用条件不同，因此分别保留；可以点选对象查看具体条件与来源。',temporal_transition:'适用时间呈先后变化；可以点选对象核对各自的时间与依据。',potential_conflict:'结构整理发现了差异，尚未确定是否冲突；不自动判定其中一条为事实。'})[edge.kind]||'来自当前整理记录的结构关系，语义含义仍需核对。';}
 function dateText(v){return v?new Date(v).toLocaleString('zh-CN',{hour12:false}):'未记录';}
 function validTime(t){return !t||t.kind==='unknown'?'适用时间未说明':String(t.start||'未说明开始')+' 至 '+String(t.end||'未说明结束');}
 function stopFrame(){if(state.frame)cancelAnimationFrame(state.frame);state.frame=0;state.lastFrame=0;}
 function resetDetail(){sourceEpoch++;state.selected=null;const detail=byId('graph-detail');detail.replaceChildren(make('h3','看看这些连接'));detail.firstChild.id='graph-detail-heading';detail.append(make('p','点选一个亮点或一条关系，也可以在列表中选择。每份理解都能回到依据。'));delete detail.dataset.nodeId;delete detail.dataset.edgeId;}
 function clearProjection(message,mode='loading'){
  state.data=null;state.next=null;state.points=[];state.lines=[];panel.dataset.state=mode;delete panel.dataset.generation;delete panel.dataset.nodeCount;delete panel.dataset.edgeCount;
  byId('graph-counts').replaceChildren();byId('graph-list').replaceChildren();byId('graph-list-count').textContent='';byId('graph-visual-count').textContent='0 个实际对象';byId('graph-status').textContent=message;byId('graph-changes').textContent='';byId('graph-more').hidden=true;byId('graph-first').hidden=!state.cursor;byId('graph-page-note').textContent='';byId('graph-empty').hidden=mode!=='empty';resetDetail();stopFrame();draw();
 }
 function fingerprint(data){return {generation:data.generation,nodes:new Map(data.nodes.map(n=>[n.id,JSON.stringify([n.object_ref,n.members,n.source_refs,n.member_count,n.protected_count,n.source_count])])),edges:new Set(data.edges.map(e=>e.id))};}
 function showChanges(before,data){const after=fingerprint(data);if(before&&before.generation!==after.generation){let added=0,removed=0,changed=0;for(const [id,refs] of after.nodes){if(!before.nodes.has(id))added++;else if(before.nodes.get(id)!==refs)changed++;}for(const id of before.nodes.keys())if(!after.nodes.has(id))removed++;const edgeAdded=[...after.edges].filter(id=>!before.edges.has(id)).length,edgeRemoved=[...before.edges].filter(id=>!after.edges.has(id)).length;byId('graph-changes').textContent='本页变化：新增 '+added+' 个对象，移除 '+removed+' 个对象，'+changed+' 个对象的版本或依据更新；关系新增 '+edgeAdded+' 条、移除 '+edgeRemoved+' 条。';}else if(state.mutationPending)byId('graph-changes').textContent='本次写入已完成，当前可见投影已重新校验。';state.baseline=after;state.mutationPending=false;}
 function setBusy(busy){for(const id of ['graph-refresh','graph-more','graph-first'])byId(id).disabled=busy;}
 function invalidate(message='材料或版本已变化，正在重新读取记忆空间。'){
  if(state.data&&!state.cursor)state.baseline=fingerprint(state.data);state.mutationPending=true;state.epoch++;state.busy=false;setBusy(false);clearTimeout(refreshTimer);clearProjection(message);if(active()){clearTimeout(mutationTimer);mutationTimer=setTimeout(()=>loadGraph(true),150);}
 }
 function checkProjection(data){
  if(data?.schema_version!=='nova.memory-graph.v0.1'||typeof data.generation!=='string'||!Array.isArray(data.nodes)||!Array.isArray(data.edges)||data.nodes.length>512||data.edges.length>1024||data.stats?.count_scope!=='page')throw new Error('invalid graph projection');
  const ids=new Set();for(const node of data.nodes){if(typeof node.id!=='string'||ids.has(node.id)||!kindLabels[node.kind]||typeof node.label!=='string'||!Array.isArray(node.members)||!Array.isArray(node.source_refs))throw new Error('invalid graph node');ids.add(node.id);}
  const edges=new Set();for(const edge of data.edges){if(typeof edge.id!=='string'||edges.has(edge.id)||!ids.has(edge.from)||!ids.has(edge.to)||typeof edge.kind!=='string')throw new Error('invalid graph edge');edges.add(edge.id);}
  return data;
 }
 async function loadGraph(clear=false){
  if(!active()||state.busy)return;const epoch=++state.epoch;state.busy=true;clearTimeout(refreshTimer);setBusy(true);
  const old=state.data,baseline=state.baseline;if(clear)clearProjection('正在读取当前可见的对象与关系…');
  try{
   const params=new URLSearchParams({limit:'2'});if(state.cursor)params.set('cursor',state.cursor);
   const data=checkProjection(await api('/graph?'+params));if(epoch!==state.epoch||!active())return;
   const changed=!old||old.generation!==data.generation;if(clear||changed){resetDetail();state.data=data;state.next=data.next_cursor||null;render(data);showChanges(baseline,data);}
   else{state.data=data;state.next=data.next_cursor||null;panel.dataset.state=data.nodes.length?'ready':'empty';updateStatus();showChanges(baseline,data);}
  }catch(error){if(epoch!==state.epoch||!active())return;state.baseline=null;state.mutationPending=false;clearProjection(state.cursor?'这一页已变化或暂时无法读取。已清除旧投影，请返回第一页或重试。':'记忆空间暂时无法读取。旧投影与来源内容已清除，请检查服务后重试。','error');}
  finally{if(epoch===state.epoch){state.busy=false;setBusy(false);if(active())refreshTimer=setTimeout(()=>loadGraph(false),8000);}}
 }
 function updateStatus(){
  if(!state.data)return;const data=state.data;const notices=[...new Set((data.notices||[]).filter(notice=>!commonNotices.has(notice)).map(notice=>noticeLabels[notice]||'部分对象暂未展示，可在记忆库查看。'))];
  byId('graph-status').textContent=(data.partial?'当前为部分投影。':'当前可见对象已读取。')+' 本页仅含普通私密与可分享、有效且未撤回的记录；与全库总数不同。'+(notices.length?' '+notices.join(' '):'');
  byId('graph-more').hidden=!state.next;byId('graph-first').hidden=!state.cursor;byId('graph-page-note').textContent='第 '+state.page+' 页 · 每页最多 2 个主题；翻页会替换投影'+(data.next_cursor?'，后面仍有记录。':'。')+' 结构化内容与当前来源会再次校验。';
 }
 function render(data){
  panel.dataset.state=data.nodes.length?'ready':'empty';panel.dataset.generation=data.generation;panel.dataset.nodeCount=data.nodes.length;panel.dataset.edgeCount=data.edges.length;
  const stats=data.stats;byId('graph-counts').replaceChildren();for(const [key,label]of [['sources','来源'],['extraction_records','提取记录'],['memories','精确归并'],['topics','主题'],['relationships','实际关系']]){const span=make('span');span.append(make('strong',Number.isSafeInteger(stats[key])?stats[key]:0),document.createTextNode(label));byId('graph-counts').append(span);}
  const topics=data.nodes.filter(n=>n.kind==='topic'),select=byId('graph-topic');select.replaceChildren(new Option('当前页全部主题',''));for(const node of topics)select.add(new Option(displayLabel(node),node.id));if(!topics.some(n=>n.id===state.topic))state.topic='';select.value=state.topic;
  updateStatus();renderVisible();
 }
 function visibleData(){
  if(!state.data)return {nodes:[],edges:[]};let nodes=state.data.nodes,edges=state.data.edges;
  if(state.topic){const ids=new Set([state.topic]);for(const edge of edges)if(edge.kind==='in_topic'&&(edge.from===state.topic||edge.to===state.topic))ids.add(edge.from===state.topic?edge.to:edge.from);for(const edge of edges)if(edge.kind==='supported_by'&&(ids.has(edge.from)||ids.has(edge.to))){ids.add(edge.from);ids.add(edge.to);}nodes=nodes.filter(n=>ids.has(n.id));}
  if(state.search){const query=state.search.toLocaleLowerCase();nodes=nodes.filter(n=>[displayLabel(n),n.label,n.summary,n.id,jsonText(n.conditions)].filter(Boolean).join(' ').toLocaleLowerCase().includes(query));}
  const ids=new Set(nodes.map(n=>n.id));edges=edges.filter(e=>ids.has(e.from)&&ids.has(e.to));return {nodes,edges};
 }
 function renderVisible(){
  const {nodes,edges}=visibleData();byId('graph-visual-count').textContent=nodes.length+' 个实际对象 · '+edges.length+' 条关系';byId('graph-empty').hidden=nodes.length>0;byId('graph-empty').textContent=state.search?'当前页没有匹配对象。请修改关键词或翻页。':state.topic?'当前页的所选主题没有可展示对象，请调整筛选或查看记忆库。':state.data&&(state.data.partial||state.data.next_cursor||state.cursor||(state.data.notices||[]).some(notice=>/omitted|partial|pending|blocked/.test(notice)))?'当前页没有可展示对象；可继续翻页或前往记忆库查看。':'当前可见范围还没有形成脉络。先留住一段记忆吧。';
  renderList(nodes,edges);draw();startFrame();
 }
 function renderList(nodes,edges){
  const list=byId('graph-list'),items=state.tab==='nodes'?nodes:edges;list.replaceChildren();list.setAttribute('aria-label',state.tab==='nodes'?'记忆对象列表':'实际关系列表');byId('graph-list-count').textContent='本页显示 '+items.length+' '+(state.tab==='nodes'?'个对象':'条关系');
  for(const item of items){const li=make('li'),button=make('button');button.type='button';button.className='graph-list-item';button.setAttribute('aria-pressed',String(state.selected?.id===item.id));
   if(state.tab==='nodes'){button.dataset.graphNodeId=item.id;button.append(make('span',kindLabels[item.kind],'graph-kind'),document.createTextNode(displayLabel(item)));button.append(make('small',item.kind==='memory'?statementNature(item)+' · '+item.member_count+' 条提取记录 · '+item.source_count+' 份来源'+(item.protected_count?' · 含你的纠正':''):item.kind==='source'?'来源元数据 · 原文按需读取':'当前主题视图'));button.addEventListener('click',()=>selectNode(item.id,true));}
   else{button.dataset.graphEdgeId=item.id;button.append(make('span',edgeLabels[item.kind]||'结构关系','graph-kind'),document.createTextNode(nodeLabel(item.from)+' → '+nodeLabel(item.to)));button.append(make('small',edgeExplanation(item)));button.addEventListener('click',()=>selectEdge(item.id,true));}li.append(button);list.append(li);
  }if(!items.length)list.append(make('li',state.data?'当前筛选没有'+(state.tab==='nodes'?'对象':'关系')+'。':'读取后会显示与图一致的列表。','graph-list-empty'));
 }
 function nodeLabel(id){const node=state.data?.nodes.find(n=>n.id===id);return node?displayLabel(node):'当前对象';}
 function dlField(dl,label,value){dl.append(make('dt',label),make('dd',value));}
 function detailBase(title,kind){sourceEpoch++;const detail=byId('graph-detail');detail.replaceChildren(make('span',kind,'graph-kind'),make('h3',title));detail.querySelector('h3').id='graph-detail-heading';delete detail.dataset.nodeId;delete detail.dataset.edgeId;return detail;}
 function selectNode(id,focus=false){
  if(!state.data)return;const node=state.data.nodes.find(n=>n.id===id);if(!node)return;state.selected={type:'node',id};const detail=detailBase(kindLabels[node.kind],kindLabels[node.kind]);detail.dataset.nodeId=node.id;detail.append(make('p',displayLabel(node),'graph-detail-label'));if(node.kind==='topic')detail.append(make('p','同一主体与属性的结构分区，不代表项目权限或已经验证的语义聚类。'));
  const dl=make('dl');dlField(dl,'语义性质',node.kind==='source'?'来源元数据；原文不参与图上自动预览':'AI 提取或结构规则形成，语义尚未独立验证');if(node.privacy_level)dlField(dl,'隐私范围',privacyLabels[node.privacy_level]||node.privacy_level);if(node.kind==='memory'){dlField(dl,'陈述性质',statementNature(node));dlField(dl,'结构主题',node.label);dlField(dl,'提取与归并',node.member_count+' 条提取记录归并为这一个对象；'+node.source_count+' 份来源');dlField(dl,'用户纠正',node.protected_count?'保留 '+node.protected_count+' 个纠正版本':'暂无');}if(node.origin)dlField(dl,'形成方式',originLabels[node.origin]||'形成方式尚未识别');if(node.valid_time)dlField(dl,'适用时间',validTime(node.valid_time));if(node.occurred_at)dlField(dl,'材料所述时间',dateText(node.occurred_at));if(node.conditions&&Object.keys(node.conditions).length)dlField(dl,'适用条件',Object.entries(node.conditions).map(([key,value])=>key+'：'+jsonText(value)).join('；'));if(node.object_ref)dlField(dl,'对象版本',node.object_ref.object_id+' · v'+node.object_ref.revision);dlField(dl,'对象标识',node.id);detail.append(dl);
  const relationEdges=state.data.edges.filter(e=>e.from===id||e.to===id);if(relationEdges.length){detail.append(make('h3','相关脉络'));const ul=make('ul');for(const edge of relationEdges){const li=make('li'),button=make('button',(edgeLabels[edge.kind]||'结构关系')+' · '+nodeLabel(edge.from===id?edge.to:edge.from),'secondary');button.type='button';button.dataset.graphEdgeId=edge.id;button.addEventListener('click',()=>selectEdge(edge.id));li.append(button);ul.append(li);}detail.append(ul);}
  if(node.members.length){detail.append(make('h3','提取版本与依据'));for(const ref of node.members){const button=make('button','查看提取版本 '+ref.revision+' · '+short(ref.object_id),'secondary');button.type='button';button.dataset.graphMemberId=ref.object_id;button.addEventListener('click',()=>loadMember(ref,button));detail.append(button);}}
  const refs=node.kind==='source'&&node.object_ref?[node.object_ref]:node.source_refs;if(refs.length){detail.append(make('h3','返回原始来源'));for(const ref of refs){const button=make('button','读取原文 v'+ref.revision+' · '+short(ref.object_id),'secondary');button.type='button';button.dataset.graphSourceId=ref.object_id;button.addEventListener('click',()=>loadSource(ref,button));detail.append(button);}}
  detail.append(make('p','位置只用于展示，不表示语义距离；图中的关联不授予客户端权限。','graph-read-boundary'));selectionChanged();if(focus)detail.focus({preventScroll:true});
 }
 function selectEdge(id,focus=false){
  if(!state.data)return;const edge=state.data.edges.find(e=>e.id===id);if(!edge)return;state.selected={type:'edge',id};const detail=detailBase(edgeLabels[edge.kind]||'结构关系','实际关系');detail.dataset.edgeId=id;detail.append(make('p',edgeExplanation(edge),'graph-detail-label'));for(const nodeId of [edge.from,edge.to]){const button=make('button',nodeLabel(nodeId),'secondary');button.type='button';button.dataset.graphNodeId=nodeId;button.addEventListener('click',()=>selectNode(nodeId));detail.append(button);}const dl=make('dl');dlField(dl,'关系标识',edge.id);dlField(dl,'判断边界',edge.kind==='supported_by'?'原文来源链接，不等于语义事实已被核实。':edge.kind==='in_topic'?'同一结构主题的归属，不是动态读取权限。':'来自当前仍有效的结构整理记录，不推定真实世界因果或确定冲突。');detail.append(dl);selectionChanged();if(focus)detail.focus({preventScroll:true});
 }
 function selectionChanged(){const {nodes,edges}=visibleData();renderList(nodes,edges);draw();}
 function guardDetail(epoch,generation,id){return active()&&epoch===sourceEpoch&&state.data?.generation===generation&&state.selected?.id===id;}
 async function loadSource(ref,button){
  const epoch=++sourceEpoch,generation=state.data?.generation,id=state.selected?.id;const detail=byId('graph-detail');for(const control of detail.querySelectorAll('[data-graph-source-id],[data-graph-member-id]'))control.disabled=false;button.disabled=true;const old=detail.querySelector('.graph-source-content');if(old)old.remove();const output=make('div',undefined,'graph-source-content');output.append(make('p','正在读取并校验来源…','graph-source-status'));detail.append(output);
  try{const source=await api('/sources/'+encodeURIComponent(ref.object_id));if(!guardDetail(epoch,generation,id))return;if(source.source?.object_id!==ref.object_id||source.source?.revision!==ref.revision)throw new Error('source version changed');output.replaceChildren(make('h3','原始材料 · v'+ref.revision),make('pre',source.content),make('small','原文仅在本页当前选择中展示，不存入浏览器缓存。'));}
  catch{if(!guardDetail(epoch,generation,id))return;invalidate('来源已变化或当前不可读。旧投影与原文已清除，正在更新。');}
  finally{if(guardDetail(epoch,generation,id))button.disabled=false;}
 }
 async function loadMember(ref,button){
  const epoch=++sourceEpoch,generation=state.data?.generation,id=state.selected?.id;const detail=byId('graph-detail');for(const control of detail.querySelectorAll('[data-graph-source-id],[data-graph-member-id]'))control.disabled=false;button.disabled=true;const old=detail.querySelector('.graph-member-content');if(old)old.remove();const output=make('div',undefined,'graph-member-content');output.append(make('p','正在读取当前版本与依据…'));detail.append(output);
  try{const result=await api('/units/'+encodeURIComponent(ref.object_id)+'/history');if(!guardDetail(epoch,generation,id))return;const versions=result.versions||[],unit=versions.find(u=>u.header?.object?.revision===ref.revision);if(!unit||versions.some(u=>u.header?.object?.revision>ref.revision))throw new Error('unit version changed');output.replaceChildren(make('h3','提取版本 '+ref.revision),make('p',jsonText(unit.content?.value)),make('small',statementNature(unit.content||{})),make('small',originLabels[unit.origin]||'形成方式尚未识别'));output.append(make('small','已保留 '+versions.length+' 个历史版本 · 当前适用时间 '+validTime(unit.valid_time)));for(const evidence of unit.evidence||[])output.append(make('blockquote',evidence.quote));output.append(make('small','这是提取与原文依据，语义支持仍未独立验证。'));}
  catch{if(!guardDetail(epoch,generation,id))return;invalidate('提取版本已变化或当前不可读。旧投影与依据已清除，正在更新。');}
  finally{if(guardDetail(epoch,generation,id))button.disabled=false;}
 }
 function hash(id){let value=2166136261;for(let i=0;i<id.length;i++)value=Math.imul(value^id.charCodeAt(i),16777619);return value>>>0;}
 function spherePoint(id){const h=hash(id),z=(h/4294967295)*1.8-.9,phi=(hash(id+'a')/4294967295)*Math.PI*2,r=Math.sqrt(1-z*z);return {x:r*Math.cos(phi),y:z,z:r*Math.sin(phi)};}
 function projected(point,width,height){const c=Math.cos(state.angle),s=Math.sin(state.angle),x=point.x*c+point.z*s,z=point.z*c-point.x*s;const radius=Math.min(width*.39,height*.34);return {x:width*.5+x*radius,y:height*.51+point.y*radius,z,depth:(z+1)/2};}
 function draw(){
  if(!ctx)return;const width=canvas.clientWidth||400,height=canvas.clientHeight||420,dpr=Math.min(window.devicePixelRatio||1,2);if(canvas.width!==Math.round(width*dpr)||canvas.height!==Math.round(height*dpr)){canvas.width=Math.round(width*dpr);canvas.height=Math.round(height*dpr);}ctx.setTransform(dpr,0,0,dpr,0,0);ctx.clearRect(0,0,width,height);state.points=[];state.lines=[];if(!active())return;
  // Unconnected, low-contrast grid is orientation geometry, never relationship data.
  if(!state.listOnly){ctx.strokeStyle='#59788c22';ctx.lineWidth=.65;for(let line=0;line<5;line++){ctx.beginPath();for(let step=0;step<=64;step++){const t=step/64*Math.PI*2,phi=line/5*Math.PI,p={x:Math.cos(t)*Math.cos(phi),y:Math.sin(t),z:Math.cos(t)*Math.sin(phi)},screen=projected(p,width,height);if(step)ctx.lineTo(screen.x,screen.y);else ctx.moveTo(screen.x,screen.y);}ctx.stroke();}for(const y of [-.5,0,.5]){ctx.beginPath();for(let step=0;step<=64;step++){const t=step/64*Math.PI*2,r=Math.sqrt(1-y*y),screen=projected({x:Math.cos(t)*r,y,z:Math.sin(t)*r},width,height);if(step)ctx.lineTo(screen.x,screen.y);else ctx.moveTo(screen.x,screen.y);}ctx.stroke();}}
  if(!state.data)return;const {nodes,edges}=visibleData(),positions=new Map();for(const node of nodes){const p=projected(spherePoint(node.id),width,height);positions.set(node.id,p);state.points.push({...p,id:node.id,node});}
  for(const edge of edges){const a=positions.get(edge.from),b=positions.get(edge.to);if(!a||!b)continue;const selected=state.selected?.id===edge.id||state.selected?.id===edge.from||state.selected?.id===edge.to;ctx.beginPath();ctx.moveTo(a.x,a.y);ctx.lineTo(b.x,b.y);ctx.strokeStyle=selected?'#bee4ffcc':edge.kind==='potential_conflict'?'#d5ae7880':edge.kind==='supported_by'?'#789db451':'#a4c7da66';ctx.lineWidth=selected?1.7:.75;ctx.setLineDash(edge.kind==='potential_conflict'?[3,4]:[]);ctx.stroke();state.lines.push({a,b,id:edge.id});}ctx.setLineDash([]);
  for(const point of [...state.points].sort((a,b)=>a.z-b.z)){const selected=state.selected?.id===point.id,r=(point.node.kind==='topic'?4.2:point.node.kind==='memory'?3.4:2.5)*(0.8+point.depth*.35);ctx.beginPath();ctx.arc(point.x,point.y,selected?r+3:r,0,Math.PI*2);ctx.fillStyle=selected?'#f0f9ff':colors[point.node.kind];ctx.globalAlpha=.5+point.depth*.5;if(selected){ctx.shadowColor='#a1d2fa';ctx.shadowBlur=12;}ctx.fill();ctx.shadowBlur=0;ctx.globalAlpha=1;if(selected){ctx.strokeStyle='#9dd6ff88';ctx.lineWidth=1;ctx.beginPath();ctx.arc(point.x,point.y,r+8,0,Math.PI*2);ctx.stroke();}}
 }
 function startFrame(){if(state.paused||state.listOnly||!state.data||!active()||state.frame)return;state.frame=requestAnimationFrame(frame);}
 function frame(timestamp){state.frame=0;if(state.paused||state.listOnly||!active())return;if(state.lastFrame)state.angle+=(timestamp-state.lastFrame)*.000035;state.lastFrame=timestamp;draw();startFrame();}
 function pause(){state.paused=true;stopFrame();byId('graph-motion').textContent='播放转动';byId('graph-motion').setAttribute('aria-pressed','false');}
 function hitLine(x,y,a,b){const dx=b.x-a.x,dy=b.y-a.y,t=Math.max(0,Math.min(1,((x-a.x)*dx+(y-a.y)*dy)/(dx*dx+dy*dy||1)));return Math.hypot(x-(a.x+t*dx),y-(a.y+t*dy));}
 canvas.addEventListener('click',event=>{if(!state.data)return;const rect=canvas.getBoundingClientRect(),x=event.clientX-rect.left,y=event.clientY-rect.top;const node=state.points.map(p=>({...p,distance:Math.hypot(x-p.x,y-p.y)})).filter(p=>p.distance<12).sort((a,b)=>a.distance-b.distance||b.z-a.z)[0];if(node){pause();selectNode(node.id);return;}const edge=state.lines.map(line=>({...line,distance:hitLine(x,y,line.a,line.b)})).filter(line=>line.distance<4).sort((a,b)=>a.distance-b.distance)[0];if(edge){pause();selectEdge(edge.id);}});
 byId('graph-refresh').addEventListener('click',()=>{state.busy=false;state.epoch++;loadGraph(true);});
 byId('graph-topic').addEventListener('change',()=>{state.epoch++;state.busy=false;sourceEpoch++;state.topic=byId('graph-topic').value;setBusy(false);state.baseline=null;resetDetail();renderVisible();if(active()){clearTimeout(refreshTimer);refreshTimer=setTimeout(()=>loadGraph(false),8000);}});
 byId('graph-search').addEventListener('input',()=>{sourceEpoch++;state.search=byId('graph-search').value.trim();state.baseline=null;resetDetail();renderVisible();});
 byId('graph-motion').setAttribute('aria-pressed','false');byId('graph-motion').addEventListener('click',()=>{state.paused=!state.paused;byId('graph-motion').textContent=state.paused?'播放转动':'暂停转动';byId('graph-motion').setAttribute('aria-pressed',String(!state.paused));if(state.paused)stopFrame();else startFrame();});
 byId('graph-list-toggle').addEventListener('click',()=>{state.listOnly=!state.listOnly;byId('graph-list-toggle').setAttribute('aria-pressed',String(state.listOnly));byId('graph-list-toggle').textContent=state.listOnly?'显示记忆球':'仅看列表';byId('graph-visual').parentElement.classList.toggle('list-only',state.listOnly);if(state.listOnly)stopFrame();else{draw();startFrame();}});
 for(const tab of ['nodes','edges'])byId('graph-'+tab+'-tab').addEventListener('click',()=>{state.tab=tab;for(const type of ['nodes','edges'])byId('graph-'+type+'-tab').setAttribute('aria-pressed',String(type===tab));const {nodes,edges}=visibleData();renderList(nodes,edges);});
 byId('graph-more').addEventListener('click',()=>{if(state.busy||!state.next)return;state.baseline=null;state.cursor=state.next;state.page++;state.topic='';loadGraph(true);});byId('graph-first').addEventListener('click',()=>{if(state.busy)return;state.baseline=null;state.cursor=null;state.page=1;state.topic='';loadGraph(true);});
 function activityChanged(){if(active()){loadGraph(!state.data);}else{state.epoch++;state.busy=false;state.baseline=null;state.mutationPending=false;clearTimeout(refreshTimer);clearTimeout(mutationTimer);clearProjection('回到记忆空间时，重新读取当前可见记录。');setBusy(false);}}
 window.addEventListener('nova:viewchange',activityChanged);window.addEventListener('nova:intake-mutation',()=>{if(state.cursor)state.baseline=null;invalidate();state.cursor=null;state.page=1;});document.addEventListener('visibilitychange',activityChanged);window.addEventListener('pagehide',()=>{state.epoch++;state.baseline=null;clearProjection('页面已离开。');});byId('lock').addEventListener('click',()=>{state.epoch++;state.baseline=null;clearProjection('已锁定。');});new MutationObserver(activityChanged).observe(byId('workspace'),{attributes:true,attributeFilter:['hidden']});
 reduced.addEventListener('change',()=>{if(reduced.matches)pause();});if(reduced.matches)byId('graph-motion').setAttribute('aria-label','当前减少动态效果，转动默认暂停；可手动播放');
 if(window.ResizeObserver)new ResizeObserver(()=>{if(active())draw();}).observe(canvas);else window.addEventListener('resize',()=>{if(active())draw();});
 window.NovaMemoryGraph=Object.freeze({invalidate,refresh:()=>loadGraph(true)});activityChanged();
})();
