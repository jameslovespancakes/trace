// TypeScript 7 compiler API adapter (trace-owned; derived from the codepath_v3 worker).
// Nothing of the project is executed: an in-memory compiler host over the snapshot sources,
// the project's configuration texts, read-only node_modules mappings and the
// compiler-bundled standard libraries (project.mjs).
//
// One-shot:  node main.mjs <input.json> <sdk>      -> prints the output object
// Serve:     node main.mjs --serve <sdk>            -> JSON lines on stdin / stdout:
//   {"op":"open","input":<input>}                             -> {"ok":true}
//   {"op":"update","changed":[{path,text}],"deleted":[path]}  -> {"ok":true}
//   {"op":"analyze","query":[paths],"names":[names]}          -> the output object (those files only)
//   {"op":"references","query":{file,start_byte}}             -> references-mode output
//   {"op":"shutdown"}                                         -> {"ok":true}, then exit
//   any failure                                               -> {"ok":false,"error":"..."}
// The session keeps one compiler API process; `update` feeds edited files through
// api.updateSnapshot({fileChanges}) on the next analysis, so only changed files are re-read.
//
// Input  {workspace, repo_root?, files: [{path, source}], configs?: [{path, text}],
//         modules?: [{virtual, real}], bundled_types?: [{virtual, real}], names?: [...],
//         query?: [paths], references?: {file, start_byte}}
// Output {symbols, edges, unresolved, uses, diagnostics, metrics, callback_params,
//         library_files, library_calls}                                   (analysis mode)
//        {references: [{file, start_byte, end_byte, line, is_declaration}],
//         references_complete, diagnostics}                                (references mode)
//
// Analysis mode (trace SPEC section 8.5 "Complete references"):
// * every function-like node with a body and every class is a symbol `ts:<file>:<start>:<end>`;
//   function expressions / arrows are named from their binding exactly like trace-syntax,
//   otherwise `<anonymous@<start>>` (mapped by span onto the syntax `<lambda>`);
// * module-level (and class-body) code is visited with the owner `ts:<file>:module`;
// * calls (call, `new`, tagged template): resolved signature -> edge (calls / constructor /
//   iterates / creates_generator), the checker rules of analyze.mjs (require, class without
//   constructor, assigned and returned functions, getters), a signature declared outside the
//   repository -> `external_signature`, else `unresolved_or_external_signature`;
// * uses: identifiers whose text is a declared name, not in callee position and not a
//   declaration name, whose checker symbol (aliases followed) is declared in the snapshot.
import fs from 'node:fs';
import readline from 'node:readline';
import {openProject} from './project.mjs';
import {analyze} from './analyze.mjs';

const args=process.argv.slice(2);
if(args[0]==='--serve'){
  await serve(args[1]);
} else {
  const [inputPath, sdk] = args;
  const input = JSON.parse(fs.readFileSync(inputPath, 'utf8'));
  const ctx = await openProject(input, sdk);
  try {
    console.log(JSON.stringify(analyze(ctx,{names:input.names,query:input.query,references:input.references})));
  } finally {ctx.api.close();}
}

async function serve(sdk){
  const write=obj=>new Promise(resolve=>{process.stdout.write(JSON.stringify(obj)+'\n',()=>resolve());});
  const lines=readline.createInterface({input:process.stdin,crlfDelay:Infinity});
  let ctx=null;
  for await (const line of lines){
    if(!line.trim())continue;
    let message;
    try{message=JSON.parse(line);}catch(e){await write({ok:false,error:'malformed request: '+e});continue;}
    try{
      switch(message.op){
        case 'open':
          if(ctx)ctx.api.close();
          ctx=await openProject(message.input,sdk);
          await write({ok:true});
          break;
        case 'update':
          if(!ctx)throw Error('no open project');
          ctx.applyChanges(message.changed??[],message.deleted??[]);
          await write({ok:true});
          break;
        case 'analyze':
          if(!ctx)throw Error('no open project');
          await write(analyze(ctx,{names:message.names,query:message.query}));
          break;
        case 'references':
          if(!ctx)throw Error('no open project');
          await write(analyze(ctx,{references:message.query}));
          break;
        case 'shutdown':
          if(ctx)ctx.api.close();
          ctx=null;
          await write({ok:true});
          process.exit(0);
          break;
        default:
          await write({ok:false,error:'unknown op '+message.op});
      }
    }catch(e){
      await write({ok:false,error:String(e&&e.stack||e)});
    }
  }
  if(ctx)ctx.api.close();
}
