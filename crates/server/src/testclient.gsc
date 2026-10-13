// Menu choices of a bot: pick a team, then a stock class each time it is about to spawn. The bot starts at level
// time `notBefore` (the stock scripts add their test clients after `wait 5`).
TestClient(team, notBefore)
{
	self endon("disconnect");

	while (gettime() < notBefore)
		wait 0.05;

	while (!isdefined(self.pers["team"]))
		wait 0.05;

	self notify("menuresponse", game["menu_team"], team);
	wait 0.5;

	names = getarraykeys(level.classMap);
	stock = [];
	for (i = 0; i < names.size; i++)
	{
		if (!issubstr(names[i], "custom") && isdefined(level.default_perk[level.classMap[names[i]]]))
			stock[stock.size] = names[i];
	}

	for (;;)
	{
		if (!level.oldschool)
			self notify("menuresponse", "changeclass", stock[randomint(stock.size)]);

		self waittill("spawned_player");
		wait 0.1;
	}
}
