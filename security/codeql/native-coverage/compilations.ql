/** Enumerate compiler-observed source files, excluding source-only extraction. */
import cpp

from Compilation compilation
where not compilation.buildModeNone()
select compilation.getAFileCompiled().getRelativePath() as compiled_file
